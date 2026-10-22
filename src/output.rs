//! Format de sortie JSON Lines et résumé.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use serde::Serialize;

use crate::hash::Stats;
use crate::scan::FileEntry;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub md5: Option<String>,
    /// Numéro du groupe de doublons (null si le fichier est unique)
    pub dup_group: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Record {
    pub fn from_entry(
        f: &FileEntry,
        key: Option<String>,
        md5: Option<String>,
        dup_group: Option<u32>,
        error: Option<String>,
    ) -> Self {
        Self {
            path: f.path.to_string_lossy().into_owned(),
            size_bytes: Some(f.size),
            modified: f.modified.map(fmt_time),
            key,
            md5,
            dup_group,
            error,
        }
    }

    pub fn error(path: &Path, error: String) -> Self {
        Self {
            path: path.to_string_lossy().into_owned(),
            size_bytes: None,
            modified: None,
            key: None,
            md5: None,
            dup_group: None,
            error: Some(error),
        }
    }
}

fn fmt_time(t: SystemTime) -> String {
    DateTime::<Local>::from(t)
        .format("%Y-%m-%dT%H:%M:%S")
        .to_string()
}

/// Doublons d'abord (par groupe), puis fichiers uniques, puis erreurs ; par chemin ensuite.
pub fn sort(records: &mut [Record]) {
    records.sort_by(|a, b| {
        let rank = |r: &Record| (r.error.is_some(), r.dup_group.is_none(), r.dup_group);
        rank(a).cmp(&rank(b)).then_with(|| a.path.cmp(&b.path))
    });
}

pub fn write(records: &[Record], out: Option<&Path>) -> Result<()> {
    let sink: Box<dyn Write> = match out {
        Some(p) => {
            Box::new(File::create(p).with_context(|| format!("création de {}", p.display()))?)
        }
        None => Box::new(io::stdout().lock()),
    };
    let mut w = BufWriter::with_capacity(1 << 16, sink);
    for r in records {
        serde_json::to_writer(&mut w, r)?;
        w.write_all(b"\n")?;
    }
    w.flush()?;
    Ok(())
}

pub fn print_summary(s: &Stats, elapsed: Duration) {
    let secs = elapsed.as_secs_f64();
    let speed = if secs > 0.0 {
        s.bytes_hashed as f64 / secs
    } else {
        0.0
    };
    eprintln!(
        "{} fichiers, {} hachés ({}, {}/s), {} groupes de doublons, {} fichiers en trop, {} récupérables, {} erreurs, {:.1} s",
        s.files,
        s.hashed,
        human_bytes(s.bytes_hashed),
        human_bytes(speed as u64),
        s.groups,
        s.duplicates,
        human_bytes(s.wasted_bytes),
        s.errors,
        secs
    );
}

/// 18,6 Go — base 1024, comme l'Explorateur Windows.
pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["o", "Ko", "Mo", "Go", "To"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} o")
    } else {
        format!("{v:.1} {}", UNITS[i]).replace('.', ",")
    }
}
