//! dupkey : identifie les doublons exacts de fichiers.
//!
//! Clé d'identité = taille + BLAKE3 (256 bits) du contenu complet.
//! Sortie : une ligne JSON par fichier (JSON Lines) ; progression et résumé sur stderr.

mod hash;
mod output;
mod scan;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use clap::{Parser, ValueEnum};

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    /// Ne hache que les candidats aux doublons et ne sort que les doublons
    Dup,
    /// Calcule la clé de tous les fichiers (inventaire)
    All,
}

/// Identifie les doublons exacts de fichiers (clé = taille + BLAKE3).
#[derive(Parser, Debug)]
#[command(name = "dupkey", version, about)]
struct Cli {
    /// Dossiers (ou fichiers) à analyser
    #[arg(required_unless_present = "files_from")]
    paths: Vec<PathBuf>,

    /// Liste de fichiers à analyser, un chemin par ligne (UTF-8) ; « - » = entrée standard
    #[arg(long, value_name = "FICHIER")]
    files_from: Option<PathBuf>,

    /// dup : seulement les doublons ; all : clé pour tous les fichiers
    #[arg(long, value_enum, default_value_t = Mode::Dup)]
    mode: Mode,

    /// Extensions à garder, séparées par des virgules (ex. mkv,mp4,jpg)
    #[arg(long, value_delimiter = ',')]
    ext: Vec<String>,

    /// Motif glob à ignorer, répétable (ex. --exclude "*.tmp" --exclude "**/Cache/**")
    #[arg(long)]
    exclude: Vec<String>,

    /// Taille minimale (ex. 1024, 500KB, 1MB, 2GB ; base 1024)
    #[arg(long, default_value = "1", value_parser = parse_size)]
    min_size: u64,

    /// Nombre de threads (par défaut : tous les cœurs)
    #[arg(long)]
    threads: Option<usize>,

    /// Fichier de sortie JSONL (par défaut : sortie standard)
    #[arg(long)]
    out: Option<PathBuf>,

    /// Ajoute aussi un MD5 (pour comparer avec d'anciens inventaires)
    #[arg(long)]
    md5: bool,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Erreur : {e:#}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    if let Some(n) = cli.threads {
        if n == 0 {
            bail!("--threads doit être supérieur à 0");
        }
        rayon::ThreadPoolBuilder::new()
            .num_threads(n)
            .build_global()
            .context("configuration des threads")?;
    }

    let start = Instant::now();
    let filter = scan::Filter::new(&cli.ext, &cli.exclude, cli.min_size)?;
    let listed = match &cli.files_from {
        Some(src) => read_file_list(src)?,
        None => Vec::new(),
    };
    let scanned = scan::collect(&cli.paths, &listed, &filter)?;
    eprintln!(
        "{} fichiers retenus ({}) en {:.1} s",
        scanned.files.len(),
        output::human_bytes(scanned.files.iter().map(|f| f.size).sum()),
        start.elapsed().as_secs_f64()
    );

    let report = hash::run(scanned.files, cli.mode, cli.md5);

    let mut records = report.records;
    records.extend(scanned.errors);
    output::sort(&mut records);
    output::write(&records, cli.out.as_deref())?;

    output::print_summary(&report.stats, start.elapsed());
    Ok(())
}

/// Lit une liste de chemins (un par ligne) depuis un fichier ou l'entrée standard (« - »).
fn read_file_list(src: &Path) -> Result<Vec<PathBuf>> {
    let mut text = String::new();
    if src == Path::new("-") {
        std::io::stdin()
            .read_to_string(&mut text)
            .context("lecture de la liste de fichiers sur l'entrée standard")?;
    } else {
        text = std::fs::read_to_string(src)
            .with_context(|| format!("lecture de la liste {}", src.display()))?;
    }
    Ok(text
        .lines()
        .map(|l| l.trim_start_matches('\u{feff}').trim_end_matches('\r'))
        .filter(|l| !l.trim().is_empty())
        .map(PathBuf::from)
        .collect())
}

/// Convertit « 1024 », « 500KB », « 1.5 GB »… en octets (base 1024).
pub fn parse_size(s: &str) -> Result<u64, String> {
    let t = s.trim().to_ascii_uppercase().replace(',', ".");
    let split = t
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(t.len());
    let (num, unit) = t.split_at(split);
    let value: f64 = num.parse().map_err(|_| format!("taille invalide : {s}"))?;
    let mult: u64 = match unit.trim() {
        "" | "B" | "O" => 1,
        "K" | "KB" | "KIB" | "KO" => 1 << 10,
        "M" | "MB" | "MIB" | "MO" => 1 << 20,
        "G" | "GB" | "GIB" | "GO" => 1 << 30,
        "T" | "TB" | "TIB" | "TO" => 1 << 40,
        u => return Err(format!("unité inconnue « {u} » dans {s}")),
    };
    Ok((value * mult as f64).round() as u64)
}

#[cfg(test)]
mod tests {
    use super::parse_size;

    #[test]
    fn tailles() {
        assert_eq!(parse_size("1024"), Ok(1024));
        assert_eq!(parse_size("1KB"), Ok(1024));
        assert_eq!(parse_size("1 mb"), Ok(1 << 20));
        assert_eq!(parse_size("1,5GB"), Ok(3 << 29));
        assert_eq!(parse_size("2Go"), Ok(2 << 30));
        assert!(parse_size("12XB").is_err());
        assert!(parse_size("abc").is_err());
    }
}
