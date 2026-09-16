![DROSTJE. — Cootje stuurt lokale coding agents en de Factory aan](assets/drostje-readme-header.webp)

# DROSTJE.

*Het stuurhuis voor de agents op je machine.*

DROSTJE. is een bureaublad-app waarmee je de coding agents op je eigen computer aanstuurt. Je zet ze aan het werk, kijkt mee terwijl ze bezig zijn, en stuurt grotere klussen door naar de Factory. Alles op één plek, en alles blijft van jou: je eigen repo's, je eigen tools, je eigen accounts.

## Waarom "drost"?

Een **drost** (of drossaard, als je het deftig wilt zeggen) was vroeger de rentmeester van de heer. Degene die de boel draaiende hield als de baas er even niet was. Precies dat doet DROSTJE. voor jou: jij zegt wat er moet gebeuren, DROSTJE. zorgt dat het gebeurt en houdt in de gaten hoe het loopt.

## Wat je ermee doet

- 🗂️ **Je projecten bij elkaar houden.** Gewoon je bestaande git-repo's, geen gedoe.
- **Meerdere threads openhouden.** Conversations en Factory-threads staan naast elkaar in tabs; de open tabs worden bewaard.
- **Prompts met Neovim schrijven.** Echte normal-, insert- en visual-modi, operators, registers en undo/redo, ingebed in de bestaande composer zonder regelnummers of terminalvenster.
- **Factory-runs starten.** Een bestaande User Story of PRD/issues-map kiezen, launch-parameters instellen en daarna de native events en resultaten volgen.
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

De standaardmap is `~/Library/Application Support/Drost`. Eén instantie mag die map tegelijk gebruiken. Projecten, threads, concepten, instellingen en launch-verwijzingen staan in SQLite; sessies, bijlagen en logs blijven bestanden. Archiveren verwijdert niets. Deze nieuwe versie gebruikt schema 2 zonder migraties; een niet-ondersteunde database wordt geweigerd, niet vervangen.

Kies in Settings zo nodig expliciete executable-paden en de gedeelde Factory-data-root. OMP draait met je eigen lokale rechten; Factory beheert zijn eigen containers en Git-worktrees. Sluiten stopt interactieve sessies, maar niet reeds gestarte Factory-processen. Een onbekende uitkomst wordt nooit automatisch opnieuw verstuurd.

### Threads en werkruimte

Projects bevat één gedeelde lijst met twee soorten threads:

- **Conversation:** een interactieve OMP-sessie met geschiedenis, berichtballonnen, bijlagen met previews en een ingebedde Neovim-prompteditor met GPUI Kit-styling. Het model kies je onderaan de composer.
- **Factory:** parameters voor een bestaande Azure DevOps User Story of een bestaande PRD/issues-map. `Advanced` bevat de branch en het optionele thought level. `Start run` slaat de definitieve configuratie eerst op; een opslagfout start niets.

Na de eerste duurzame launch wordt de Factory-thread een alleen-lezen monitor met native events, resultaten en ruwe details. Ook een geweigerde launch blijft als historie bewaard. Een volgende poging begint in een **nieuwe Factory-thread**. Er is geen taakpakket-generator, handoff vanuit een bericht of Factory-composer.

De werkruimte heeft Projects links, threadtabs centraal en een optioneel Output-dock onderaan. Je kunt de native docks verplaatsen, groeperen en schalen; de indeling wordt bewaard. Conversations en Factory-threads blijven tegelijk open in de tabstrip van het Thread-dock; bij te veel tabs biedt het overloopmenu toegang tot de rest. `Close thread` sluit alleen die tab en eventueel een inactieve Conversation-sessie, niet de historie of een lopende turn of Factory-run. Bij sluiten van de actieve tab wordt een aangrenzende tab geselecteerd. Concepttekst blijft behouden bij wisselen, sluiten en opnieuw openen.

Zonder geopende thread zie je compacte startacties in het midden; een lege tabstrip wordt niet getoond. Opslagstatus staat in de titelbalk, niet in een aparte onderbalk. Output blijft bereikbaar via de dockkop, `Cmd-J` en de command palette.

### Toetsenbord

|Toets|Actie|
|---|---|
|`Cmd-Shift-P`|Command palette: opdrachten, projecten en beide threadsoorten|
|`Cmd-N` / `Cmd-Shift-N`|Nieuwe Conversation / Factory-thread|
|`Cmd-1` / `Cmd-2` / `Cmd-3`|Focus Projects / Thread / Output; opent zo nodig het dock|
|`Cmd-B` / `Cmd-J`|Projects / Output tonen of verbergen|
|`Ctrl-Tab` / `Ctrl-Shift-Tab`|Volgende / vorige open threadtab|
|`Cmd-W`|Huidige threadtab sluiten|
|`Ctrl-W`, daarna `h/j/k/l`|Naar een zichtbaar aangrenzend paneel; tweede toets mag ook met Control|
|`j/k`, pijlen, `gg/G`, `h/l`, Return|Navigeren, springen, invouwen/uitvouwen en openen in Projects|
|`i` in een Conversation|Focus naar de prompteditor|
|`Cmd-Enter`|Prompt versturen, Factory-configuratie starten of formulier bevestigen|
|`Cmd-.`|Interactieve turn onderbreken|
|`Cmd-Escape`|Van de prompteditor terug naar Thread|
|`Cmd-,`|Settings|
|Escape|Bovenste menu of venster sluiten; in de palette eerst de zoektekst wissen|

In insert-modus voegt Return een nieuwe regel toe; alleen `Cmd-Enter` verstuurt. Escape schakelt binnen de editor de Neovim-modus terug; `Cmd-Escape` verlaat de editor. Moties, operators, visual-selectie, registers, zoeken en undo/redo worden door Neovim uitgevoerd. `Cmd-C/X/V` gebruikt het systeemklembord. Directionele paneeltoetsen nemen geen editorinvoer over.

Versturen neemt een gecontroleerde snapshot nadat Neovim de eerdere invoer verwerkt heeft. Tijdens actieve tekstcompositie of een onvoltooide Neovim-opdracht wordt niet verstuurd of naar een andere draft gewisseld. Onvoltooide IME-tekst blijft aparte recoverytekst; die wordt nooit automatisch verstuurd of in een projectbestand geschreven.

Ontbreekt Neovim of valt het proces weg, dan verschijnt een fout zonder terugval naar een tweede editor. De command palette biedt `Restart Neovim editor`, `Cancel editor command` en `Copy draft recovery`. Herstart herstelt laatst waargenomen concepttekst, niet nog onbevestigde toetsaanslagen; registers en undo-historie van het oude proces gaan verloren. Annuleren bewaart onvoltooide compositietekst apart voor herstel. Een aangepast Neovim-pad geldt bij de volgende appstart, of bij opnieuw proberen na een mislukte eerste editorstart.

### Uiterlijk

Settings groepeert de bestaande instellingen en kan zoeken op naam, beschrijving en groep. Het biedt **System**, **Light** en **Dark**, met een afzonderlijke themakeuze per modus. Save past de keuze toe; Cancel verandert het uiterlijk niet. De Neovim-prompteditor volgt dezelfde achtergrond-, tekst-, selectie- en cursorkleuren en het monospace-lettertype.

Eigen thema's staan in `<DROST_DATA_DIR>/themes` (standaard `~/Library/Application Support/Drost/themes`). Settings toont de map en kan die openen. Gebruik GPUI Kit `ThemeSet`-JSON: een object met een `themes`-array; iedere entry heeft een unieke `name` en `mode` (`light` of `dark`). Dit is niet het losse Zed-themaformaat. Alleen Default Light en Default Dark zijn ingebouwd.

Toegevoegde en gewijzigde bestanden worden opnieuw geladen. Ontbreekt het gekozen thema, dan gebruikt Drost een standaardthema met een waarschuwing; de opgeslagen naam blijft behouden zodat herstel van het bestand de keuze kan herstellen. Gebruik unieke namen om dubbele registry-entries te vermijden.

### Vereisten en controle

GPUI Kit is vastgezet op `0.6.1`. De prompteditor vereist een geïnstalleerde Neovim met de benodigde embedded-RPC- en UI-API's; getest met **Neovim 0.12.3** op macOS. Het pad wordt via PATH ontdekt of expliciet in Settings opgegeven. Drost start Neovim zonder gebruikers-init, plugins, ShaDa, swap- of undobestanden en gebruikt alleen eigen scratchbuffers. Dit is geen OS-sandbox: expliciete Neovim-opdrachten draaien met je lokale rechten.

De OMP-integratie is getest met `18.2.0` (native RPC v2); authenticatie en modeltoegang moeten al beschikbaar zijn. Factory vereist daarnaast een compatibele CLI, Docker, de agent-image en de authenticatie/broker van de gekozen harness.

`Check readiness` toont echte `factory doctor --harness …`-uitvoer. Een Factory-thread gebruikt `opencode` (standaard), `codex` of `claude-code`; interactieve Conversations blijven OMP gebruiken. Een ongeldige of niet-ondersteunde keuze blijft zichtbaar en moet vóór Start worden aangepast. DROSTJE. voert geen upgrades of authenticatie uit.

```sh
cargo check --all-targets
cargo test
cargo test --test neovim_runtime -- --ignored --nocapture
cargo test --features runtime-qa native_composition_is_recovery_only_until_committed -- --ignored --nocapture
```

### Expliciete desktop-QA

Maak eerst de wegwerp-repository `FX` uit [het applicatieplan](DROST_APPLICATION_PLAN.md). De normale tests gebruiken geen betaalde provider-turns. De onderstaande GPUI-QA gebruikt echte geïsoleerde processen, maar vervangt geen visuele of OS-inputcontrole.

```sh
export DROST_RUNTIME_FIXTURE="$FX"
cargo test --features runtime-qa --lib workspace_actions_with_real_backends -- --ignored --nocapture
cargo test --features runtime-qa --lib editor_startup_failure_and_restart_preserve_draft -- --ignored --nocapture
cargo test --features runtime-qa --lib thread_workspace_keyboard_and_themes -- --ignored --nocapture
cargo test --features runtime-qa --lib empty_workspace_actions_stay_compact -- --ignored --nocapture
cargo test --features runtime-qa --lib orderly_quit_flushes_before_platform_deadline -- --ignored --nocapture
cargo test --features runtime-qa --lib factory_start_requires_persisted_configuration -- --ignored --nocapture
```

Deze scenario's versturen geen provider-turns en starten geen remote Factory-run. Ze controleren native draftwissels, nieuwe regels, bescherming tegen verouderde acknowledgements, openen/sluiten/herstellen van threadtabs, beide threadsoorten, palette-/focusgedrag, themawijzigingen, veilige afsluiting en de opslagbarrière vóór Factory Start.

Gebruik de door de eerste QA-test gemelde datamap als `DROST_QA_DATA` voor layout/herstart-controle:

```sh
cargo test --features runtime-qa --lib layout_and_factory_runtime_recovery -- --ignored --nocapture
```

De hersteltest heropent alleen de opgeslagen werkruimte. Als de fixture al launch-records bevat, worden die via de echte eventtracker in hun eigen Factory-thread getoond, zonder opnieuw te dispatchen. Exitcode 0 zonder terminal event blijft `Interrupted / Unknown`, niet Completed.

Een eerdere echte OpenCode-proef heeft dispatch, sandbox-initialisatie en GUI-herstart/herstel doorlopen. Die run pauzeerde met `PermissionDenied` / `SandboxNonZero`: Factory wees een ACP-verzoek van de agent naar `/proc/sys/kernel/random/uuid` af. Die proef bewijst geen voltooide run; de sandbox-policy is niet aangepast en deze UI-QA herhaalt die remote proef niet.

## Onder de motorkap

Rust + GPUI, macOS. Gesprekken lopen via OMP, prompttekst bewerk je met ingebedde Neovim binnen de GPUI Kit-composer, en klussen gaan via de `factory` CLI naar de Compliance Software Factory. Nodig: Rust, `nvim`, `omp`, `factory`.

---

*Koffie erbij? Dan gaan we.*
