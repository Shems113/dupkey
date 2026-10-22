//! Tests de bout en bout : on génère de vrais fichiers et on lance l'exécutable.

use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

fn write(dir: &Path, name: &str, data: &[u8]) {
    let p = dir.join(name);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, data).unwrap();
}

fn run(args: &[&str]) -> Vec<Value> {
    let out = Command::new(env!("CARGO_BIN_EXE_dupkey"))
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn name(v: &Value) -> String {
    Path::new(v["path"].as_str().unwrap())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

/// Gros fichier pseudo-aléatoire (le pré-filtre partiel s'applique au-delà de 128 Ko)
fn big(seed: u8, len: usize) -> Vec<u8> {
    let mut x = (seed as u32).wrapping_mul(2_654_435_761).wrapping_add(1);
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect()
}

#[test]
fn detecte_les_vrais_doublons_et_seulement_eux() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    let a = big(1, 1_000_000);

    write(d, "a1.mkv", &a);
    write(d, "sous/dossier/a2.mkv", &a); // vrai doublon
    let mut a_milieu = a.clone();
    a_milieu[500_000] ^= 1; // même taille, même début et même fin : seul le hash complet tranche
    write(d, "a_milieu.mkv", &a_milieu);
    write(d, "petit1.txt", b"bonjour");
    write(d, "petit2.txt", b"bonjour"); // doublon de petite taille
    write(d, "petit3.txt", b"bonsoir"); // même taille, contenu différent
    write(d, "unique.jpg", &big(9, 300_000));

    let recs = run(&[d.to_str().unwrap()]);
    let mut names: Vec<String> = recs.iter().map(name).collect();
    names.sort();
    assert_eq!(names, ["a1.mkv", "a2.mkv", "petit1.txt", "petit2.txt"]);

    // Groupe 1 = le plus gros gain (le fichier de 1 Mo)
    let g = |n: &str| {
        recs.iter().find(|r| name(r) == n).unwrap()["dupGroup"]
            .as_u64()
            .unwrap()
    };
    assert_eq!(g("a1.mkv"), 1);
    assert_eq!(g("a2.mkv"), 1);
    assert_eq!(g("petit1.txt"), 2);
    assert!(recs[0]["key"].as_str().unwrap().starts_with("1000000:"));
}

#[test]
fn mode_all_donne_une_cle_a_chaque_fichier() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    write(d, "x.bin", b"abc");
    write(d, "y.bin", b"abc");
    write(d, "z.bin", b"xyz");

    let recs = run(&[d.to_str().unwrap(), "--mode", "all", "--md5"]);
    assert_eq!(recs.len(), 3);
    let z = recs.iter().find(|r| name(r) == "z.bin").unwrap();
    assert!(z["dupGroup"].is_null());
    // Valeurs de référence : b3sum et md5sum de « abc »
    let x = recs.iter().find(|r| name(r) == "x.bin").unwrap();
    assert_eq!(
        x["key"],
        "3:6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85"
    );
    assert_eq!(x["md5"], "900150983cd24fb0d6963f7d28e17f72");
}

#[test]
fn filtres_extension_exclusion_taille_et_dossiers_qui_se_chevauchent() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    write(d, "a.MKV", b"meme contenu");
    write(d, "b.mkv", b"meme contenu");
    write(d, "c.txt", b"meme contenu");
    write(d, "Cache/d.mkv", b"meme contenu");
    write(d, "e.mkv", b"x");
    write(d, "f.mkv", b"x");

    let sous = d.join("Cache");
    let recs = run(&[
        d.to_str().unwrap(),
        sous.to_str().unwrap(), // chevauchement : ne doit pas créer de faux doublons
        "--ext",
        ".mkv",
        "--exclude",
        "**/cache/**",
        "--min-size",
        "2",
    ]);
    let mut names: Vec<String> = recs.iter().map(name).collect();
    names.sort();
    assert_eq!(names, ["a.MKV", "b.mkv"]);
}

#[test]
fn liste_de_fichiers_sur_entree_standard() {
    use std::io::Write;
    use std::process::Stdio;

    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    write(d, "a.txt", b"pareil");
    write(d, "b.txt", b"pareil");
    write(d, "c.txt", b"pareil"); // même contenu mais absent de la liste
    write(d, "vide.txt", b"");
    let liste = format!(
        "{}\r\n{}\n\n{}\n{}\n",
        d.join("a.txt").display(),
        d.join("b.txt").display(),
        d.join("vide.txt").display(),
        d.join("absent.txt").display()
    );

    let mut child = Command::new(env!("CARGO_BIN_EXE_dupkey"))
        .args(["--mode", "all", "--min-size", "0", "--files-from", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(liste.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let recs: Vec<Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();

    let get = |n: &str| recs.iter().find(|r| name(r) == n);
    assert_eq!(recs.len(), 4);
    assert_eq!(get("a.txt").unwrap()["dupGroup"], 1);
    assert_eq!(get("b.txt").unwrap()["dupGroup"], 1);
    assert!(get("c.txt").is_none());
    assert!(get("vide.txt").unwrap()["key"]
        .as_str()
        .unwrap()
        .starts_with("0:"));
    assert!(get("absent.txt").unwrap()["error"].is_string());
}
