# worktree-prune

[![Build](https://github.com/iltumio/worktree-prune/actions/workflows/build.yml/badge.svg)](https://github.com/iltumio/worktree-prune/actions/workflows/build.yml)

CLI Rust per eliminare i worktree Git e i relativi target Cargo esterni, con una TUI per selezionarli.

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

Esegui da qualsiasi cartella: per impostazione predefinita cerca ricorsivamente le repository Git nella cartella corrente e nelle sue sottocartelle, poi mostra tutte le loro worktree in un unico inventario. Usa `-C /percorso/cartella` per scegliere un’altra radice di ricerca. Le worktree registrate vengono incluse anche se si trovano fuori dalla cartella scandita; una stessa repository viene mostrata una sola volta.

La scansione include cartelle nascoste e repository annidate, riconosce repository bare e file `.git`, non segue symlink verso sottocartelle e non entra nei metadati `.git`. Le cartelle non leggibili vengono segnalate e saltate.

```sh
worktree-prune                         # apre la TUI
worktree-prune feature                 # anteprima, nessuna cancellazione
worktree-prune feature --yes           # elimina worktree e target non condiviso
worktree-prune --list                  # tutte le repository sotto la cartella corrente
worktree-prune -C ~/dev --list         # cerca ricorsivamente in ~/dev
worktree-prune feature --yes --delete-branch
worktree-prune --stale --force         # anteprima dei residui non registrati
worktree-prune --orphans --yes         # elimina target senza proprietario
```

Nella TUI: frecce o `j`/`k` per spostarsi, **Spazio** per selezionare, `a` per selezionare/deselezionare tutti, **Invio** per aprire la modale con il piano di rimozione. Nella modale, `y` conferma direttamente la cancellazione (conferma unica); `n` o **Esc** annullano e tornano alla selezione; `q` o **Ctrl+C** escono. In modalità rimozione, il checkout principale e i worktree bloccati con `git worktree lock` non sono selezionabili. Sono visibili branch, stato dei commit, modifiche locali, dimensioni, barra dello spazio recuperabile e dettagli dell'elemento selezionato. Se la selezione presenta modifiche locali, commit non pubblicati o cartelle bloccate da permessi/proprietario, la modale mostra i motivi del blocco insieme al piano forzato (abilitando `force` e, se necessario per cartelle bloccate, `sudo` previa conferma con `y`). Se il blocco non è superabile, la modale lo segnala e non consente la conferma.

Per liberare solo lo spazio di build, premi **`c`**: la barra superiore mostra **CLEAN TARGETS — keep worktrees**. Seleziona con **Spazio** (o `a`), premi **Invio** per vedere i percorsi e **`y`** per pulirli. Worktree, branch, modifiche locali e registrazioni Git restano intatti. Funziona anche sul checkout principale, sul worktree corrente e sui worktree bloccati o con commit locali; non serve `--force`. I target condivisi o non sicuri bloccano l’intero piano. I permessi possono richiedere una conferma SUDO. Premi di nuovo `c` per tornare alla rimozione; il cambio modalità azzera la selezione.

La pulizia elimina soltanto il **target mostrato nell’inventario**, senza eseguire `cargo clean`: usa la configurazione locale o il target esterno convenzionale; se quest’ultimo manca e c’è una directory `target/` nel worktree, usa quella. Un `target/` locale con file tracciati da Git viene protetto. Non vengono cercati altri percorsi tramite configurazioni Cargo globali o variabili d’ambiente.

Tutte le opzioni dello script sono disponibili: `-n/--dry-run`, `-y/--yes`, `-f/--force`, `-s/--sudo`, `-q/--quiet`, `--keep-target`, `--delete-branch`, `--list`, `--stale`, `--orphans`. In aggiunta: `-C/--repo`, `--version`. Il comando senza argomenti apre la TUI; qualsiasi argomento o flag usa la CLI senza TUI. `--tui` non è necessario e non è accettato. Senza terminale, il comando senza argomenti suggerisce `--list`. Tra `--yes` e `--dry-run` prevale l'ultima opzione. `--keep-target` e `--orphans` sono incompatibili.

## Protezioni e differenze dallo script

- Nella CLI il dry-run è predefinito; i flag senza selezione mostrano l'elenco. Nella TUI nessuna cancellazione avviene prima della selezione e della conferma con `y` nella modale.
- Modifiche locali, file non tracciati e commit non raggiungibili da un branch remoto o da `main`/`master` impediscono la rimozione, salvo `--force`. I riferimenti remoti sono quelli locali: non viene eseguito un fetch automatico.
- Una vecchia PR merged non dimostra che la punta attuale del branch sia al sicuro. Il porting non interroga `gh`; anche uno squash merge può quindi richiedere `--force` dopo verifica manuale.
- I residui non registrati richiedono sempre `--force`: anche senza `.git` possono contenere file non salvati altrove. I cloni indipendenti non sono candidati.
- Checkout principale, worktree bloccati, directory di lavoro corrente e antenati restano protetti anche con `--force`. Per eliminare il worktree in cui ti trovi, spostati prima nel checkout principale.
- Un errore nei controlli blocca **l'intero piano**. Gli errori durante l'esecuzione interrompono le operazioni successive; le cancellazioni già completate non possono essere annullate. Dopo la conferma TUI inventario e controlli vengono riletti.
- Durante la rimozione dei worktree, i target condivisi, sovrapposti ad altri worktree, fuori dalla base o appartenenti al checkout principale vengono conservati. I percorsi sono risolti anche attraverso symlink e `..`. Il target `main` non viene raccolto come orfano.
- La base è `CARGO_TARGET_BASE_DIR`, oppure `/.cargo-targets/targets`. La chiave resta `<repo>/<nome-worktree>/target`, con `main` per il checkout principale, compatibile con `cargo-target-provision`. `[build].target-dir` viene letto come TOML; `.cargo/config` ha precedenza su `.cargo/config.toml`. Le configurazioni Cargo globali, ereditate o le variabili `CARGO_TARGET_DIR` non vengono usate per scoprire ulteriori target da cancellare.
- Proprietà e permessi vengono controllati prima della rimozione. `--sudo` (o la conferma `FORCE + SUDO` nella TUI) abilita `sudo rm` solo quando necessario. Non vengono rimossi worktree ancora registrati se Git rifiuta la rimozione. Non viene eseguito un `git worktree prune` globale su registrazioni estranee al piano.
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

I controlli indipendenti (inventario delle repository, stato Git, dimensioni dei target e verifiche del piano) vengono eseguiti in parallelo con Rayon. Ordine delle righe e diagnostica restano stabili; le cancellazioni sono sequenziali e iniziano solo dopo tutti i controlli. Per limitare il parallelismo, usa ad esempio `WTP_THREADS=4 worktree-prune`.


## Configurazione

All’avvio viene letto il file utente `$XDG_CONFIG_HOME/worktree-prune/config.toml`, oppure `~/.config/worktree-prune/config.toml` quando `XDG_CONFIG_HOME` è assente, vuoto o relativo (convenzioni XDG). Il file è opzionale; non viene creato automaticamente.

```toml
# Numero di thread per i controlli paralleli (intero maggiore di zero).
threads = 4
```

Precedenza: `WTP_THREADS` → `threads` nel TOML → parallelismo disponibile del sistema. `RAYON_NUM_THREADS` non viene più usata. Un file malformato, una chiave sconosciuta o un numero di thread non valido vengono segnalati prima della scansione; `--help` e `--version` restano disponibili. Esempio pronto da copiare in `config.example.toml`.
