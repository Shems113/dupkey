//! Calcul des clés : filtre par taille, hash partiel, puis BLAKE3 complet.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use md5::{Digest, Md5};
use memmap2::Mmap;
use rayon::prelude::*;

use crate::output::{self, Record};
use crate::scan::FileEntry;
use crate::Mode;

/// Taille lue au début et à la fin du fichier pour le pré-filtre
const PARTIAL: u64 = 64 * 1024;
/// Au-delà, BLAKE3 découpe aussi le fichier entre plusieurs threads
const RAYON_THRESHOLD: usize = 1 << 20;

#[derive(Default)]
pub struct Stats {
    pub files: usize,
    pub hashed: usize,
    pub bytes_hashed: u64,
    pub groups: usize,
    pub duplicates: usize,
    pub wasted_bytes: u64,
    pub errors: usize,
}

pub struct Report {
    pub records: Vec<Record>,
    pub stats: Stats,
}

struct Full {
    key: String,
    md5: Option<String>,
    bytes: u64,
}

pub fn run(files: Vec<FileEntry>, mode: Mode, with_md5: bool) -> Report {
    let mut stats = Stats {
        files: files.len(),
        ..Default::default()
    };
    let mut records = Vec::new();

    // 1. Fichiers à hacher entièrement
    let to_hash: Vec<FileEntry> = match mode {
        Mode::All => files,
        Mode::Dup => {
            let candidates = keep_shared(files, |f| f.size);
            let (big, small): (Vec<_>, Vec<_>) =
                candidates.into_iter().partition(|f| f.size > 2 * PARTIAL);

            // 2. Pré-filtre : début + fin du fichier (jamais utilisé comme clé)
            let partial: Vec<(FileEntry, io::Result<[u8; 32]>)> = big
                .into_par_iter()
                .map(|f| {
                    let h = partial_hash(&f.path);
                    (f, h)
                })
                .collect();
            let mut ok = Vec::new();
            for (f, h) in partial {
                match h {
                    Ok(h) => ok.push((f, h)),
                    Err(e) => records.push(Record::from_entry(
                        &f,
                        None,
                        None,
                        None,
                        Some(e.to_string()),
                    )),
                }
            }
            let survivors = keep_shared(ok, |(f, h)| (f.size, *h));
            small
                .into_iter()
                .chain(survivors.into_iter().map(|(f, _)| f))
                .collect()
        }
    };

    // 3. BLAKE3 complet, avec progression
    let total: u64 = to_hash.iter().map(|f| f.size).sum();
    let done = Arc::new(AtomicU64::new(0));
    let progress = Progress::start(total, done.clone());
    let hashed: Vec<(FileEntry, io::Result<Full>)> = to_hash
        .into_par_iter()
        .map(|f| {
            let r = full_hash(&f.path, with_md5);
            done.fetch_add(f.size, Ordering::Relaxed);
            (f, r)
        })
        .collect();
    progress.stop();

    // 4. Regroupement par clé
    let mut by_key: HashMap<String, Vec<(FileEntry, Full)>> = HashMap::new();
    for (f, r) in hashed {
        match r {
            Ok(full) => {
                stats.hashed += 1;
                stats.bytes_hashed += full.bytes;
                by_key.entry(full.key.clone()).or_default().push((f, full));
            }
            Err(e) => records.push(Record::from_entry(
                &f,
                None,
                None,
                None,
                Some(e.to_string()),
            )),
        }
    }

    // Groupes numérotés du plus gros gain au plus petit
    let mut groups: Vec<Vec<(FileEntry, Full)>> = by_key.into_values().collect();
    groups.sort_by_cached_key(|g| {
        let wasted = (g.len() as u64 - 1) * g[0].0.size;
        (std::cmp::Reverse(wasted), g[0].1.key.clone())
    });

    let mut next_id = 1u32;
    for group in groups {
        let is_dup = group.len() > 1;
        if !is_dup && mode == Mode::Dup {
            continue; // collision de pré-filtre seulement
        }
        let id = if is_dup {
            stats.groups += 1;
            stats.duplicates += group.len() - 1;
            stats.wasted_bytes += (group.len() as u64 - 1) * group[0].0.size;
            next_id += 1;
            Some(next_id - 1)
        } else {
            None
        };
        for (f, full) in group {
            records.push(Record::from_entry(&f, Some(full.key), full.md5, id, None));
        }
    }

    stats.errors = records.iter().filter(|r| r.error.is_some()).count();
    Report { records, stats }
}

/// Garde les éléments dont la clé est partagée par au moins un autre élément.
fn keep_shared<T, K: std::hash::Hash + Eq>(items: Vec<T>, key: impl Fn(&T) -> K) -> Vec<T> {
    let mut map: HashMap<K, Vec<T>> = HashMap::new();
    for it in items {
        map.entry(key(&it)).or_default().push(it);
    }
    map.into_values()
        .filter(|v| v.len() > 1)
        .flatten()
        .collect()
}

fn partial_hash(path: &Path) -> io::Result<[u8; 32]> {
    let mut f = File::open(path)?;
    let mut buf = vec![0u8; PARTIAL as usize];
    let mut h = blake3::Hasher::new();
    f.read_exact(&mut buf)?;
    h.update(&buf);
    f.seek(SeekFrom::End(-(PARTIAL as i64)))?;
    f.read_exact(&mut buf)?;
    h.update(&buf);
    Ok(*h.finalize().as_bytes())
}

fn full_hash(path: &Path, with_md5: bool) -> io::Result<Full> {
    let file = File::open(path)?;
    let len = file.metadata()?.len();

    let mut b3 = blake3::Hasher::new();
    let mut md5 = with_md5.then(Md5::new);

    // Lecture mappée en mémoire (rapide sur SSD) ; lecture classique en secours
    // SAFETY : lecture seule ; si le fichier est modifié pendant le hachage,
    // la clé est fausse pour ce fichier mais aucune mémoire n'est corrompue.
    let mapped = if len > 0 {
        unsafe { Mmap::map(&file) }.ok()
    } else {
        None
    };
    let bytes = match &mapped {
        Some(m) => {
            if m.len() >= RAYON_THRESHOLD {
                b3.update_rayon(m);
            } else {
                b3.update(m);
            }
            if let Some(h) = md5.as_mut() {
                h.update(&m[..]);
            }
            m.len() as u64
        }
        None => {
            let mut reader = io::BufReader::with_capacity(1 << 20, file);
            let mut buf = vec![0u8; 1 << 20];
            let mut n_total = 0u64;
            loop {
                let n = reader.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                b3.update(&buf[..n]);
                if let Some(h) = md5.as_mut() {
                    h.update(&buf[..n]);
                }
                n_total += n as u64;
            }
            n_total
        }
    };

    Ok(Full {
        key: format!("{bytes}:{}", b3.finalize().to_hex()),
        md5: md5.map(|h| hex(&h.finalize())),
        bytes,
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Affiche l'avancement sur stderr (seulement dans un terminal).
struct Progress {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Progress {
    /// Dans un terminal : une ligne réécrite sur place toutes les 0,5 s.
    /// Sinon (lancé par un autre programme, sortie redirigée) : une ligne complète
    /// toutes les 5 s, lisible dans un fichier de log.
    fn start(total: u64, done: Arc<AtomicU64>) -> Self {
        use std::io::IsTerminal;
        let stop = Arc::new(AtomicBool::new(false));
        let terminal = std::io::stderr().is_terminal();
        let handle = (total > 0).then(|| {
            let stop = stop.clone();
            std::thread::spawn(move || {
                let period = if terminal { 1 } else { 10 }; // en pas de 0,5 s
                let mut tick = 0u32;
                while !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(500));
                    tick += 1;
                    if !tick.is_multiple_of(period) || stop.load(Ordering::Relaxed) {
                        continue;
                    }
                    let d = done.load(Ordering::Relaxed);
                    let line = format!(
                        "Hachage : {} / {} ({:.0} %)",
                        output::human_bytes(d),
                        output::human_bytes(total),
                        d as f64 * 100.0 / total as f64
                    );
                    if terminal {
                        eprint!("\r{line}   ");
                    } else {
                        eprintln!("{line}");
                    }
                }
                if terminal {
                    eprint!("\r{:60}\r", "");
                }
            })
        });
        Self { stop, handle }
    }

    fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle {
            let _ = h.join();
        }
    }
}
