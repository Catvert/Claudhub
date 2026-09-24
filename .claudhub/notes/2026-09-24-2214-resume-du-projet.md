---
claudhub: note
anchor: repo
title: Claudhub en quelques lignes
author: Arno Dupont
agent: claude
created: 2026-09-24T22:14:27+02:00
---
**Claudhub** est un client git de bureau en Rust (gpui), pensé pour travailler à plusieurs worktrees à la fois, avec des agents Claude Code dans des terminaux intégrés.

- **Trois couches** : `git/`, `db/`, `terminal/`… sans interface ; des workers qui répondent aux `Cmd` par des `Evt` sur sept files ; `ui/` seule connaît gpui et ne fait aucune entrée-sortie.
- **Une seule aire de dock** : la gauche choisit (fichiers, changements, tests), la droite se souvient (bases, Sentry, GitHub), le bas s'étale (terminaux, graphe), le centre se lit (diff, éditeur, consoles SQL).
- **L'accueil** : un plan zoomable où chaque dépôt est un arbre — worktrees, terminaux vivants, notes en fichiers Markdown versionnés.
- **Cibles** : Linux natif (AppImage, paquet nix) et Windows, où l'interface `.exe` pilote un `claudhub-server` headless dans WSL2.
- **Extensions** : le `justfile`, le `wt.toml` du projet et les commandes des réglages — pas de plugins.
