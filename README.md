# worktree-prune

[![Build](https://github.com/iltumio/worktree-prune/actions/workflows/build.yml/badge.svg)](https://github.com/iltumio/worktree-prune/actions/workflows/build.yml)

CLI Rust per eliminare i worktree Git e i relativi target Cargo esterni, con una TUI per selezionarli. Porting dello script installato in `~/.local/bin/worktree-prune`, conservato in `legacy/worktree-prune.sh` come riferimento; il binario non lo esegue.

## Installazione

Richiede Rust/Cargo (verificato con Rust 1.95.0), Git e un linker C su Linux.

```sh
git clone https://github.com/iltumio/worktree-prune.git
cd worktree-prune
./install.sh
```

Lo script compila il checkout in modalità release con `Cargo.lock` e installa in `~/.local/bin`, senza `sudo`. Se trova un eseguibile diverso, ne conserva una copia `worktree-prune.bak.XXXXXX` prima di sostituirlo. Un errore di compilazione lascia intatta l'installazione precedente.

Per scegliere un'altra directory:

```sh
INSTALL_DIR="$HOME/bin" ./install.sh
```

La directory scelta deve essere nel `PATH`; lo script segnala se manca, senza modificare la configurazione della shell. Supporta `CARGO_TARGET_DIR` per la cache di compilazione. Per aggiornare, esegui `git pull --ff-only` nel clone e rilancia `./install.sh`.

## Uso

Esegui dal checkout principale del repository interessato, oppure usa `-C /percorso/repo`.

```sh
worktree-prune                         # apre la TUI
worktree-prune feature                 # anteprima, nessuna cancellazione
worktree-prune feature --yes           # elimina worktree e target non condiviso
worktree-prune --list                  # elenco testuale e dimensioni
worktree-prune feature --yes --delete-branch
worktree-prune --stale --force         # anteprima dei residui non registrati
worktree-prune --orphans --yes         # elimina target senza proprietario
```

Nella TUI: frecce o `j`/`k` per spostarsi, **Spazio** per selezionare, `a` per selezionare/deselezionare tutti, **Invio** per vedere il piano. Nel piano, `y` conferma la cancellazione dei soli elementi selezionati, se tutti i controlli passano; **Esc** torna alla selezione. `q` o **Ctrl+C** escono. Il checkout principale e i worktree bloccati non sono selezionabili. Sono visibili branch, stato dei commit, modifiche locali, dimensioni e percorsi. Se il piano è bloccato, una modale mostra i motivi: `y` abilita `force` per la selezione e mostra il piano forzato; un secondo `y` conferma la cancellazione. `n` o **Esc** annullano la modale. Tornando alla selezione, `force` viene disattivato. Se il blocco non è superabile con `force`, la modale lo segnala e non consente la conferma.

Tutte le opzioni dello script sono disponibili: `-n/--dry-run`, `-y/--yes`, `-f/--force`, `-s/--sudo`, `-q/--quiet`, `--keep-target`, `--delete-branch`, `--list`, `--stale`, `--orphans`. In aggiunta: `-C/--repo`, `--version`. Il comando senza argomenti apre la TUI; qualsiasi argomento o flag usa la CLI senza TUI. `--tui` non è necessario e non è accettato. Senza terminale, il comando senza argomenti suggerisce `--list`. Tra `--yes` e `--dry-run` prevale l'ultima opzione. `--keep-target` e `--orphans` sono incompatibili.

## Protezioni e differenze dallo script

- Nella CLI il dry-run è predefinito; i flag senza selezione mostrano l'elenco. Nella TUI nessuna cancellazione avviene prima della selezione, anteprima e conferma con `y`.
- Modifiche locali, file non tracciati e commit non raggiungibili da un branch remoto o da `main`/`master` impediscono la rimozione, salvo `--force`. I riferimenti remoti sono quelli locali: non viene eseguito un fetch automatico.
- Una vecchia PR merged non dimostra che la punta attuale del branch sia al sicuro. Il porting non interroga `gh`; anche uno squash merge può quindi richiedere `--force` dopo verifica manuale.
- I residui non registrati richiedono sempre `--force`: anche senza `.git` possono contenere file non salvati altrove. I cloni indipendenti non sono candidati.
- Checkout principale, worktree bloccati, directory di lavoro corrente e antenati restano protetti anche con `--force`. Per eliminare il worktree in cui ti trovi, spostati prima nel checkout principale.
- Un errore nei controlli blocca **l'intero piano**. Gli errori durante l'esecuzione interrompono le operazioni successive; le cancellazioni già completate non possono essere annullate. Dopo la conferma TUI inventario e controlli vengono riletti.
- I target condivisi, sovrapposti ad altri worktree, fuori dalla base o appartenenti al checkout principale vengono conservati. I percorsi sono risolti anche attraverso symlink e `..`. Il target `main` non viene raccolto come orfano.
- La base è `CARGO_TARGET_BASE_DIR`, oppure `/.cargo-targets/targets`. La chiave resta `<repo>/<nome-worktree>/target`, con `main` per il checkout principale, compatibile con `cargo-target-provision`. `[build].target-dir` viene letto come TOML; `.cargo/config` ha precedenza su `.cargo/config.toml`. Le configurazioni Cargo globali, ereditate o le variabili `CARGO_TARGET_DIR` non vengono usate per scoprire ulteriori target da cancellare.
- Proprietà e permessi vengono controllati prima della rimozione. `--sudo` abilita `sudo rm` solo quando necessario nella CLI. Non vengono rimossi worktree ancora registrati se Git rifiuta la rimozione. Non viene eseguito un `git worktree prune` globale su registrazioni estranee al piano.
- `--quiet` mantiene il comportamento per hook: output sintetico e stato 0 anche in caso di errore operativo. Gli errori di sintassi degli argomenti restano errori.

Il calcolo delle dimensioni con `du` avviene prima dell'apertura della TUI e può richiedere tempo per target molto grandi. Le dimensioni rappresentano byte apparenti, non una misura esatta dei blocchi liberati. Evita build o modifiche concorrenti durante una cancellazione.

## Compilazione e verifica

Richiede Linux/Unix, Rust con edition 2024, Git e `du`; `sudo` serve solo con l'opzione corrispondente. Sviluppato e verificato con Rust 1.95.0.

```sh
cargo build --release --locked
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
cargo fmt --check
python3 tests/tui.py                    # test in pseudoterminale, dopo cargo build/test
```

I test usano esclusivamente repository temporanei. Coprono dry-run, cancellazione effettiva, branch, blocchi per dati locali, batch, target condivisi/esterni, symlink, residui, orfani, nomi ambigui, percorsi insoliti e ripristino del terminale.

Implementazione: `src/main.rs` gestisce gli argomenti, `src/prune.rs` inventario/piano/rimozione, `src/tui.rs` l'interfaccia. La CLI usa [clap](https://docs.rs/clap/latest/clap/) e la TUI [Ratatui](https://docs.rs/ratatui/latest/ratatui/) con Crossterm.

## CI e licenza

GitHub Actions esegue formattazione, Clippy, test CLI e TUI e build release Linux x86_64 su push e pull request, oppure manualmente. Il binario, README e licenza sono disponibili come archivio negli artifact del workflow.

Distribuito con [licenza MIT](LICENSE).
