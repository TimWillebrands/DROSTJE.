![DROSTJE. — Cootje stuurt lokale coding agents en de Factory aan](assets/drostje-readme-header.webp)

# DROSTJE.

*Het stuurhuis voor de agents op je machine.*

DROSTJE. is een bureaublad-app waarmee je de coding agents op je eigen computer aanstuurt. Je zet ze aan het werk, kijkt mee terwijl ze bezig zijn, en stuurt grotere klussen door naar de Factory. Alles op één plek, en alles blijft van jou: je eigen repo's, je eigen tools, je eigen accounts.

## Waarom "drost"?

Een **drost** (of drossaard, als je het deftig wilt zeggen) was vroeger de rentmeester van de heer. Degene die de boel draaiende hield als de baas er even niet was. Precies dat doet DROSTJE. voor jou: jij zegt wat er moet gebeuren, DROSTJE. zorgt dat het gebeurt en houdt in de gaten hoe het loopt.

## Wat je ermee doet

- 🗂️ **Je projecten bij elkaar houden.** Gewoon je bestaande git-repo's, geen gedoe.
- 💬 **Kletsen met een coding agent.** Eén live gesprek, met geschiedenis, plaatjes en tools die je ziet werken.
- ⌨️ **Écht in Neovim typen.** Geen Vim-imitatie, maar de echte. `ciw`, `dd`, `u`, het doet het allemaal.
- 🏭 **Werk naar de Factory sturen.** Taak schrijven, nakijken, versturen. Daarna kijk je rustig hoe de run vordert.
- 📜 **Terugzien wat er gebeurd is.** Elke run blijft bewaard, ook als je DROSTJE. tussendoor even dichtdoet.

## Wat DROSTJE. níet doet

- **Niks verzinnen.** Mislukt een run, dan zie je dat. Is iets onbekend, dan staat er "onbekend".
- **Niks stiekem installeren of inloggen.** Ontbreekt er iets, dan zegt DROSTJE. dat netjes in Settings.
- **Geen editor voor je hele codebase.** Daar heb je andere dingen voor.
- **Niet zelf opnieuw proberen.** Een drost doet wat je vraagt, niet meer en niet minder.

## Aan de slag

```sh
cargo run
```

Wat ontbreekt, vertelt DROSTJE. je zelf. Wil je een aparte data-map (handig voor testen)? Zet `DROST_DATA_DIR`.

De standaardmap is `~/Library/Application Support/Drost`. Eén instantie mag die map tegelijk gebruiken. Projecten, concepten, instellingen en launch-verwijzingen staan in SQLite; sessies, bijlagen, werkpakketten en logs blijven bestanden. Archiveren verwijdert niets.

Kies in Settings zo nodig expliciete executable-paden en de gedeelde Factory-data-root. OMP en Neovim draaien met je eigen lokale rechten; Factory beheert zijn eigen containers en Git-worktrees. Sluiten stopt interactieve sessies, maar niet reeds gestarte Factory-processen. Een onbekende uitkomst wordt nooit automatisch opnieuw verstuurd.

### Vereisten en controle

Getest met GPUI Kit `0.6.1`, OMP `18.2.0` (native RPC v2) en Neovim `0.12.3` op macOS. OMP-authenticatie en modeltoegang moeten al beschikbaar zijn. Factory vereist daarnaast een compatibele CLI, Docker, de agent-image en de authenticatie/broker van de gekozen harness.

`Check Readiness` toont echte `factory doctor --harness …`-uitvoer. Het Factory-formulier laat je kiezen uit `omp`, `opencode`, `codex` en `claude-code`; de echte Factory-run-test kiest `opencode` via datzelfde keuzeveld. Interactieve gesprekken blijven OMP gebruiken. De hier aangetroffen Factory-build `de152249fb93` kent de `omp`-harness nog niet. DROSTJE. voert geen upgrades of authenticatie uit.

```sh
cargo check --all-targets
cargo test
NVIM=/opt/homebrew/bin/nvim cargo test --test neovim_runtime -- --ignored --nocapture
NVIM=/opt/homebrew/bin/nvim cargo test --lib nested_clipboard_replies_unwind_in_reverse_order -- --ignored --nocapture
```

### Expliciete desktop-QA

Maak eerst de wegwerp-repository `FX` uit [het applicatieplan](DROST_APPLICATION_PLAN.md). De normale tests gebruiken geen betaalde provider-turns. De onderstaande GPUI-QA gebruikt echte geïsoleerde processen, maar vervangt geen visuele of OS-inputcontrole.

```sh
export DROST_RUNTIME_FIXTURE="$FX"
cargo test --features runtime-qa --lib workspace_actions_with_real_backends -- --ignored --nocapture
cargo test --features runtime-qa --lib orderly_quit_flushes_before_platform_deadline -- --ignored --nocapture
```

Met `DROST_PROVIDER_SMOKE=1` controleert de eerste QA-test ook een echte afbeelding/prompt, toolgebruik, interrupt/continue en sessieherstel. Dit gebruikt je bestaande authenticatie en provider-capaciteit.

Gebruik de door de eerste QA-test gemelde datamap als `DROST_QA_DATA` voor layout/herstart-controle:

```sh
cargo test --features runtime-qa --lib layout_and_factory_runtime_recovery -- --ignored --nocapture
```

Alleen met `DROST_FACTORY_SMOKE=1` probeert die laatste test een echte Factory-dispatch met `opencode` op de fixture. Een readiness-weigering is zichtbaar en bewaart het concept; dat is geen geslaagde Factory-run. Het eerste echte macOS-venster is visueel gecontroleerd; de interacties zijn via GPUI test-support geverifieerd omdat Accessibility-automatisering niet beschikbaar was.

De echte OpenCode-proef heeft dispatch, sandbox-initialisatie en GUI-herstart/herstel doorlopen. De run pauzeerde daarna met `PermissionDenied` / `SandboxNonZero`: Factory wees een ACP-verzoek van de agent naar `/proc/sys/kernel/random/uuid` af. De completion-assertie blijft terecht falen zolang de run niet werkelijk voltooit; de sandbox-policy is niet aangepast.

## Onder de motorkap

Rust + GPUI, macOS. Gesprekken lopen via OMP, tekst typ je in een ingebedde Neovim, en klussen gaan via de `factory` CLI naar de Compliance Software Factory. Nodig: Rust, `nvim`, `omp`, `factory`.

---

*Koffie erbij? Dan gaan we.*
