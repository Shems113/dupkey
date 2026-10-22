# dupkey

Identifie les doublons exacts de fichiers. La clé d'identité est `taille:BLAKE3` (256 bits) du contenu complet : deux fichiers de même clé sont identiques octet pour octet.

## Compiler (Windows)

1. Installer Rust une fois : https://rustup.rs (choisir l'installation par défaut, avec les outils de compilation Visual Studio proposés).
2. Dans `D:\Projets\Rust\dupkey` :

```
cargo build --release
cargo test --release
```

L'exécutable est `target\release\dupkey.exe` (autonome, copiable n'importe où).

## Utilisation

```
dupkey D:\Photos E:\Sauvegarde [options]
```

| Option | Rôle | Défaut |
|---|---|---|
| `--mode dup` / `--mode all` | `dup` : seulement les doublons. `all` : clé pour tous les fichiers (inventaire) | `dup` |
| `--ext mkv,mp4,jpg` | Extensions à garder | toutes |
| `--exclude "**/Cache/**"` | Motif glob à ignorer, répétable, `/` comme séparateur | aucun |
| `--min-size 1MB` | Taille minimale (base 1024) | 1 octet |
| `--threads 8` | Nombre de threads | tous les cœurs |
| `--out doublons.jsonl` | Fichier de sortie | sortie standard |
| `--files-from liste.txt` | Fichiers à analyser, un chemin par ligne (UTF-8) ; `-` = entrée standard | aucun |
| `--md5` | Ajoute un MD5 (nettement plus lent) | non |

## Sortie (JSON Lines)

```json
{"path":"D:\\Photos\\IMG_001.jpg","sizeBytes":4182016,"modified":"2024-07-14T18:35:12","key":"4182016:0c9d…e771","dupGroup":1}
```

- `dupGroup` : numéro du groupe de doublons, `1` = plus gros gain d'espace ; `null` pour un fichier unique (mode `all`).
- `error` : présent si le fichier est illisible.
- Progression et résumé sur la sortie d'erreur.

Avec `--files-from`, un fichier absent ou illisible donne une ligne `error` sans arrêter le traitement.

Lecture depuis Python : `pd.read_json("doublons.jsonl", lines=True)`.

## Fonctionnement

1. Regroupement par taille : un fichier de taille unique n'a pas de doublon.
2. Pré-filtre sur les 64 premiers et 64 derniers Ko (jamais utilisé comme clé).
3. BLAKE3 complet des candidats restants, en parallèle, lecture mappée en mémoire.

Limite : seules les copies exactes sont détectées (une photo recompressée ou aux EXIF modifiés a une autre clé).
