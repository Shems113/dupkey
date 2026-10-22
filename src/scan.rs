//! Parcours des dossiers (en parallèle) et filtres.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::{Context, Result};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

use crate::output::Record;

pub struct FileEntry {
    pub path: PathBuf,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

pub struct ScanResult {
    pub files: Vec<FileEntry>,
    /// Fichiers ou dossiers illisibles, déjà au format de sortie
    pub errors: Vec<Record>,
}

pub struct Filter {
    exts: Option<HashSet<String>>,
    exclude: Option<Arc<GlobSet>>,
    min_size: u64,
}

impl Filter {
    pub fn new(exts: &[String], exclude: &[String], min_size: u64) -> Result<Self> {
        let exts: HashSet<String> = exts
            .iter()
            .map(|e| e.trim().trim_start_matches('.').to_lowercase())
            .filter(|e| !e.is_empty())
            .collect();

        let exclude = if exclude.is_empty() {
            None
        } else {
            let mut b = GlobSetBuilder::new();
            for pat in exclude {
                // « / » comme séparateur dans les motifs, y compris sous Windows
                let glob = GlobBuilder::new(&pat.replace('\\', "/"))
                    .case_insensitive(true)
                    .build()
                    .with_context(|| format!("motif --exclude invalide : {pat}"))?;
                b.add(glob);
            }
            Some(Arc::new(b.build()?))
        };

        Ok(Self {
            exts: (!exts.is_empty()).then_some(exts),
            exclude,
            min_size,
        })
    }

    fn keeps_file(&self, path: &Path) -> bool {
        if let Some(exts) = &self.exts {
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if !exts.contains(&ext) {
                return false;
            }
        }
        !is_excluded(self.exclude.as_deref(), path)
    }
}

fn is_excluded(set: Option<&GlobSet>, path: &Path) -> bool {
    let Some(set) = set else { return false };
    let normalized = path.to_string_lossy().replace('\\', "/");
    set.is_match(&normalized)
        || path
            .file_name()
            .is_some_and(|name| set.is_match(Path::new(name)))
}

/// `roots` : dossiers ou fichiers à parcourir (un chemin absent est une erreur fatale).
/// `listed` : fichiers donnés un par un (--files-from) ; un fichier absent ou illisible
/// devient une ligne d'erreur dans la sortie, sans arrêter le traitement.
pub fn collect(roots: &[PathBuf], listed: &[PathBuf], filter: &Filter) -> Result<ScanResult> {
    let mut files = Vec::new();
    let mut errors = Vec::new();

    for path in listed {
        match dunce::canonicalize(path) {
            Ok(p) if p.is_file() => {
                if filter.keeps_file(&p) {
                    let meta = std::fs::metadata(&p);
                    push_file(&mut files, &mut errors, p, meta, filter.min_size);
                }
            }
            Ok(p) => errors.push(Record::error(&p, "pas un fichier".into())),
            Err(e) => errors.push(Record::error(path, e.to_string())),
        }
    }

    for root in roots {
        let root = dunce::canonicalize(root)
            .with_context(|| format!("chemin introuvable : {}", root.display()))?;

        if root.is_file() {
            if filter.keeps_file(&root) {
                let meta = std::fs::metadata(&root);
                push_file(&mut files, &mut errors, root, meta, filter.min_size);
            }
            continue;
        }

        let exclude = filter.exclude.clone();
        let walker = jwalk::WalkDir::new(&root)
            .skip_hidden(false)
            .follow_links(false)
            .process_read_dir(move |_, _, _, children| {
                // Élague les dossiers exclus pour ne pas les parcourir du tout
                children.retain(|c| match c {
                    Ok(e) => !(e.file_type.is_dir() && is_excluded(exclude.as_deref(), &e.path())),
                    Err(_) => true,
                });
            });

        for entry in walker {
            match entry {
                Ok(e) if e.file_type.is_file() => {
                    let path = e.path();
                    if filter.keeps_file(&path) {
                        let meta = std::fs::symlink_metadata(&path);
                        push_file(&mut files, &mut errors, path, meta, filter.min_size);
                    }
                }
                Ok(_) => {}
                Err(err) => {
                    let path = err.path().map(Path::to_path_buf).unwrap_or_default();
                    errors.push(Record::error(&path, err.to_string()));
                }
            }
        }
    }

    // Dossiers qui se chevauchent : un même fichier ne doit compter qu'une fois
    files.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    files.dedup_by(|a, b| a.path == b.path);
    Ok(ScanResult { files, errors })
}

fn push_file(
    files: &mut Vec<FileEntry>,
    errors: &mut Vec<Record>,
    path: PathBuf,
    meta: std::io::Result<std::fs::Metadata>,
    min_size: u64,
) {
    match meta {
        Ok(m) if m.len() >= min_size => files.push(FileEntry {
            size: m.len(),
            modified: m.modified().ok(),
            path,
        }),
        Ok(_) => {}
        Err(e) => errors.push(Record::error(&path, e.to_string())),
    }
}
