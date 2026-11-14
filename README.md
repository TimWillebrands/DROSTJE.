![DROSTJE. — Cootje stuurt lokale coding agents en de Factory aan](assets/drostje-readme-header.webp)

# DROSTJE.

*Het stuurhuis voor agents op je eigen machines.*

DROSTJE. is een desktop-app (macOS en Linux) waarin je coding agents aan het werk zet op je eigen repo's. Je schrijft prompts in een ingebedde Neovim, kijkt mee terwijl de agent werkt, ziet direct wat er in je werkboom verandert en stuurt grotere klussen door naar de Compliance Software Factory. Alles draait met je eigen tools, accounts en rechten.

> Een **drost** was vroeger de rentmeester van de heer: degene die de boel draaiende hield als de baas er even niet was. Jij zegt wat er moet gebeuren; DROSTJE. zorgt dat het gebeurt en laat zien hoe het loopt.

**Inhoud:** [In het kort](#in-het-kort) · [Installeren](#installeren) · [Eerste stappen](#eerste-stappen) · [Begrippen](#begrippen) · [De werkruimte](#de-werkruimte) · [Sneltoetsen](#sneltoetsen) · [Onderdelen](#onderdelen) · [Conversations op een Host](#conversations-op-een-host) · [Uiterlijk](#uiterlijk) · [Problemen oplossen](#problemen-oplossen) · [Ontwikkelen](#ontwikkelen)

## In het kort

- **Vijf agents, één werkplek.** Conversations met OMP, Pi, Claude Code, Codex of OpenCode, naast elkaar in tabs.
- **Prompts in echte Neovim.** Modi, operators, registers en undo in de composer; concepten worden bewaard.
- **Zien wat er verandert.** De tab Changes toont de Git-diff van de werkboom live, inline of naast elkaar, en opent elke gewijzigde regel in een Neovim-tab.
- **Werk gescheiden houden.** Elke Conversation kan een eigen Git-worktree en branch krijgen; Spaces groeperen projecten.
- **Terminal, Neovim en browser ingebouwd.** Een echte Ghostty-terminal, je eigen Neovim met plugins, en een Servo-browser waarmee agents je app kunnen bedienen en controleren.
- **Agents laten samenwerken.** Agents in dezelfde Space kunnen elkaar berichten sturen, als jij dat toestaat.
- **Factory-runs starten en volgen.** Kies een User Story of PRD, start de run en volg de events.
- **Ook op andere machines.** Laat een Conversation draaien op een Host, bijvoorbeeld een Linux-buildmachine.
- **Volledig met het toetsenbord.** Vim-toetsen in elke lijst, `Cmd-Alt-h/j/k/l` tussen panelen, alles via de command palette.

Wat DROSTJE. bewust **niet** doet:

- **Niets verzinnen.** Mislukt iets, dan zie je dat. Is een uitkomst onbekend, dan staat er *Unknown*.
- **Niets vanzelf opnieuw proberen.** Geen automatische retries of herverzending na een crash of verbroken verbinding.
- **Niets stiekem installeren.** Ontbrekende tools staan als probleem in Settings. Installeren en inloggen doe je zelf.
- **Geen IDE.** DROSTJE. bewerkt geen projectbestanden; daarvoor open je Neovim of je eigen editor.

## Installeren

### Wat je nodig hebt

| Wat | Waarvoor | Nodig? |
|---|---|---|
| Rust (stable), Zig 0.16, CMake, pkg-config, Python 3, make | De app bouwen | Ja |
| Xcode (macOS) of Wayland/EGL/OpenGL, fontconfig, libxml2 en libc++ van LLVM 21+ (Linux) | Platform-SDK | Ja |
| [Neovim](https://neovim.io) 0.12+ | De prompteditor en Neovim-tabs | Ja |
| `omp`, `pi`, `claude`, `codex` of `opencode` | Conversations; minstens één, al ingelogd | Ja |
| `git` | Projecten, Changes, worktrees, commits | Ja |
| `factory` CLI, Docker en de agent-image | Factory-runs | Alleen voor Factory |
| `azdo` CLI met `az login` | Pull requests en reviews uit Azure DevOps | Optioneel |
| Node.js 22.19+ en npm | **Install Pi** in Settings | Optioneel |

De app-installer installeert geen van deze tools. Settings toont wat ontbreekt; daar kun je ook expliciete paden opgeven.

### Bouwen en installeren

```sh
./install.sh
```

- **macOS:** maakt een ongesigneerde `DROSTJE.app` in `/Applications`. Een andere map: `INSTALL_DIR="$HOME/Applications" ./install.sh`.
- **Linux:** installeert onder `~/.local` (binary in `bin/`, `.desktop`-item en iconen in `share/`). Systeembreed: `INSTALL_DIR=/usr/local ./install.sh`. Getest op Arch met COSMIC/Wayland; X11 wordt voor de terminal niet ondersteund.

Direct starten zonder installatie:

```sh
cargo run
```

De eerste build compileert Ghostty en Servo en duurt lang; daarna wordt een cache gebruikt (`GHOSTTY_NATIVE_CACHE_DIR` verplaatst die). Zig staat op `PATH` of in `ZIG=/pad/naar/zig`. Een gebouwde app heeft geen Zig of losse Ghostty nodig.

### Waar je gegevens staan

| Platform | Standaardmap |
|---|---|
| macOS | `~/Library/Application Support/Drostje` |
| Linux | `$XDG_DATA_HOME/drostje` (standaard `~/.local/share/drostje`) |

Een andere map, bijvoorbeeld om te testen: `DROSTJE_DATA_DIR=/tmp/drostje-test cargo run`. Eén instantie tegelijk per map. Projecten, threads, concepten en instellingen staan in SQLite; sessies, bijlagen en logs zijn gewone bestanden. Archiveren verwijdert nooit iets. Oudere databases (schema 2–7) worden gemigreerd; een onbekend schema wordt geweigerd, niet overschreven.

## Eerste stappen

1. **Project toevoegen.** Klik **Add project…** in Projects en kies een map met een Git-repo.
2. **Conversation starten.** Klik de `+` naast het project en kies een agent, of druk `Cmd-N` voor de standaardagent van het project.
3. **Prompt schrijven.** De composer is Neovim: begin in insert-modus te typen, Return maakt een nieuwe regel. `Cmd-Return` verstuurt.
4. **Meekijken.** De Conversation toont antwoorden, denkstappen en toolkaarten. Rechts staat de zijbalk met **Changes**: de actuele diff van je werkboom. `Cmd-.` onderbreekt de turn.
5. **Toestemming geven.** Vraagt de agent om een tool of invoer, dan beantwoord je dat op de kaart in de Conversation.
6. **Committen.** **Commit…** boven de diff opent een commitformulier met een voorgesteld bericht.

Kwijt? `Cmd-Shift-P` opent de command palette met elke opdracht en zijn sneltoets.

## Begrippen

| Begrip | Betekenis |
|---|---|
| **Space** | Een werkcontext met eigen projecten, open tabs en indeling. Je begint in *Default*. |
| **Project** | Een Git-repo op je machine. Kan in meerdere Spaces staan. |
| **Thread** | Een Conversation of een Factory thread. Hoort bij precies één project en Space. |
| **Conversation** | Een interactieve sessie met een agent (OMP, Pi, Claude Code, Codex of OpenCode). |
| **Factory thread** | Configuratie en verloop van één Factory-run. Na de start alleen-lezen. |
| **Draft** | De nog niet verstuurde prompt van een Conversation. Wordt bewaard. |
| **Turn** | Eén ronde: jouw prompt en het werk van de agent tot die klaar is. |
| **Worktree** | Een aparte Git-werkboom met een eigen branch, zodat agents elkaar niet in de weg zitten. |
| **Host** | Een andere machine waarop een Conversation draait. |

## De werkruimte

```
┌──────────┬───────────────────────────────────┬────────────────────┐
│ Projects │  [tab] [tab] [tab]                │ Changes · Output · │
│          │                                   │ Terminal · Agents ·│
│ Space ▾  │   Conversation                    │ Browser · …        │
│ project  │                                   │                    │
│  thread  │                                   │   zijbalk van      │
│  thread  │  ┌─────────────────────────────┐  │   deze thread      │
│          │  │ composer (Neovim)           │  │                    │
│          │  └─────────────────────────────┘  │                    │
└──────────┴───────────────────────────────────┴────────────────────┘
```

- **Projects** (links, `Cmd-B`) toont de projecten en threads van de actieve Space.
- **Midden** staan threads, terminals, Neovim- en browsertabs. Sleep een tab naast of onder een andere om panelen te splitsen. Open tabs en hun indeling worden bewaard.
- **Elke thread heeft een eigen zijbalk** (`Cmd-J`) met tabs. Sleep de rand om hem breder te maken; de breedte wordt per thread bewaard.
- **Titelbalk** toont opslag- en foutstatus.

Concepten worden opgeslagen zodra je de editor of een veld verlaat, van thread wisselt of iets bevestigt. Er is geen opslagtimer tijdens het typen; bij een crash kan wat je sinds die laatste bevestiging typte verloren gaan.

## Sneltoetsen

`Cmd` is Command op macOS en Control op Linux; `Alt` is Option. Op Linux zijn `Cmd-W` en `Cmd-V` `Ctrl-Shift-W` en `Ctrl-Shift-V`, zodat `Ctrl-W` en `Ctrl-V` van editors en shells blijven.

**Overal**

| Toets | Actie |
|---|---|
| `Cmd-Shift-P` | Command palette: alle opdrachten, projecten en threads |
| `Cmd-N` / `Cmd-Shift-N` | Nieuwe Conversation / Factory thread |
| `Cmd-Alt-N` | Nieuwe Conversation in een nieuwe worktree |
| `Cmd-Alt-T` | Nieuwe terminal in het geselecteerde project |
| `Cmd-Alt-O` | Bestand uit de Conversation openen in Neovim |
| `Cmd-1` / `Cmd-2` / `Cmd-3` | Focus op Projects / thread / Output |
| `Cmd-B` / `Cmd-J` | Projects / zijbalk tonen of verbergen |
| `Ctrl-Tab` / `Ctrl-Shift-Tab` | Volgende / vorige tab |
| `Cmd-Alt-]` / `Cmd-Alt-[` | Volgende / vorige Space |
| `Cmd-W` | Tab sluiten (een thread blijft bewaard; een drukke terminal vraagt eerst) |
| `Cmd-Alt-h/j/k/l` of `Cmd-Alt`-pijltjes | Naar het paneel in die richting, ook vanuit editor, terminal en browser |
| `Cmd-,` | Settings |
| Escape | Bovenste menu of venster sluiten |

**In lijsten** (Projects, Changes, Agents, kiezers): `j`/`k` of pijltjes bewegen, `gg`/`G` naar begin/eind, Return opent. In Projects klappen `h`/`l` een project in en uit, en `/` zoekt.

**In een Conversation**

| Toets | Actie |
|---|---|
| `j`/`k`, `gg`/`G`, Page Up/Down | Door de geschiedenis scrollen |
| `i` | Naar de composer |
| `Cmd-Return` | Prompt versturen |
| `Cmd-.` | Turn onderbreken |
| `Cmd-Escape` | Editor verlaten (de Draft blijft) |
| `Cmd-V` | Afbeelding van het klembord als bijlage; tekst plakt gewoon |

**In de composer** werkt Neovim zoals je gewend bent. Escape gaat terug naar normal-modus; `:q`, `:wq` en `ZZ` verlaten de editor zonder iets weg te gooien. `:w` is niet nodig.

**In Changes**: `Return` of `o` opent de geselecteerde regel in Neovim, `s` wisselt tussen inline en naast elkaar.

## Onderdelen

### Conversations en agents

Kies per Conversation een agent; de standaard per project stel je in Settings in. Het model, de effort en de toestemmingsmodus kies je onderaan de composer. Wat die modus betekent verschilt per agent: Codex kent bijvoorbeeld *Read only*, *Ask for approval*, *Auto review* en *Full access*; OpenCode kent de agents *Build* en *Plan*.

| Agent | Hoe DROSTJE. hem aanstuurt |
|---|---|
| OMP | Native RPC v2 (getest met 18.2.0) |
| Pi | JSONL-RPC |
| Claude Code | Streaming JSON; tooltoestemming in DROSTJE. |
| Codex | `codex app-server` |
| OpenCode | `opencode acp` |

Bij heropenen hervat elke agent zijn eigen sessie. Achtergrondjobs van OMP verschijnen als compacte kaarten; *Finished* betekent dat het werk stopte, niet dat het slaagde. Plak afbeeldingen met `Cmd-V` of sleep bestanden in de Conversation. Sleep de lijn boven de composer om het promptgebied groter te maken.

Valt Neovim weg, dan zie je een fout en biedt de palette **Restart Neovim editor**, **Cancel editor command** en **Copy draft recovery**. Je laatst bekende concept komt terug; er is geen tweede editor als vangnet.

### Zijbalk van een thread

| Tab | Wat je ziet |
|---|---|
| **Changes** | De niet-gecommitte Git-diff van de werkboom, live bijgewerkt. **Side by side**/**Inline** wisselt de weergave, **Open** opent de geselecteerde regel in Neovim, **Commit…** commit alles. |
| **Details** | Volledige details van een toolkaart, zonder de Conversation te blokkeren. |
| **Review** | Reviewthreads van de gekoppelde Azure DevOps-pull request (alleen als die er is). |
| **Output** | Uitvoer van een Factory-run. |
| **Terminal** | Een shell in de werkboom van deze thread. Blijft draaien bij wisselen; na een herstart begint een nieuwe shell. |
| **Agents** | Het agentnetwerk van de Space (zie hieronder). |
| **Browser** | Een blijvende browsersessie naast de Conversation. |

### Worktrees en branches

**New conversation in new worktree…** (`Cmd-Alt-N`) maakt een eigen worktree onder de datamap, op een nieuwe branch `drostje/<naam>` vanaf een basisbranch die je kiest. De branch krijgt zijn definitieve naam zodra de Conversation een titel heeft. `Cmd-N` vanuit zo'n Conversation deelt dezelfde worktree.

De balk onder de composer toont de werkruimte (*Current checkout* of de codenaam van de worktree) en de branch. Daar wissel je van branch (**Switch branch…**) of start je een Conversation in een andere werkruimte. Branch wisselen kan niet tijdens een turn of met niet-gecommitte wijzigingen.

Een worktree wordt vanzelf opgeruimd als de laatste thread is gearchiveerd en de worktree schoon en gemerged is. Anders blijft hij staan tot **Remove worktree…**.

### Terminal en Neovim

- **Terminal** (`Cmd-Alt-T`): een echte Ghostty-terminal met je eigen shell (op macOS een login-shell). Sluiten vraagt alleen bevestiging als er nog iets draait. Terminaltabs worden niet bewaard over herstarts.
- **Neovim-tab** (**New Neovim tab**): je eigen Neovim met je eigen config en plugins. Bestandspaden in een Conversation, zoals `src/app.rs:42`, zijn klikbaar en openen hier op de juiste regel. Zonder muis: `Cmd-Alt-O`. Staat het bestand al open, dan springt die tab naar de regel.

`Ctrl-C` en `Ctrl-W` blijven van de terminal en Neovim; `Cmd-Alt-h/j/k/l` werkt er gewoon doorheen.

### Factory

Een Factory thread start een run voor een bestaande Azure DevOps User Story of een PRD/issues-map. **Advanced** bevat de branch en het thought level. **Check readiness** toont de echte `factory doctor`-uitvoer. **Start run** slaat de configuratie eerst op; mislukt dat, dan start er niets.

Na de start wordt de thread een alleen-lezen monitor met events en resultaten. Een nieuwe poging begint in een nieuwe Factory thread. Exitcode 0 zonder afsluitend event telt als *Interrupted / Unknown*, niet als geslaagd. Factory-processen draaien door als je DROSTJE. sluit.

### Spaces

Een Space is een eigen werkcontext, zoals in de [Zen-browser](https://docs.zen-browser.app/user-manual/workspaces). Elke Space onthoudt zijn open tabs, selectie, ingeklapte projecten en paneelindeling. Wisselen via de Space-kiezer in Projects, **Switch space…**, `Cmd-Alt-]`/`[` of een horizontale tweevingerveeg boven Projects. Wisselen stopt geen lopende agent en bewaart concepten.

Een project kan in meerdere Spaces staan; een thread hoort bij één. Verplaats een thread met **Move thread to space**, een project met **Move project to space…**. Een verwijderde Space geeft zijn threads en projecten terug aan *Default*. Spaces scheiden organisatie, geen accounts of rechten.

### Agentnetwerk

In de tab **Agents** meld je een Conversation aan bij het netwerk van zijn Space. Agents krijgen dan de tool `network` om elkaar te vinden (`members`), een bericht te sturen (`send`) of iedereen te bereiken (`broadcast`).

> ⚠️ **Join geeft toestemming voor betaalde turns.** Is een aangemelde agent idle, dan kan een bericht van een andere agent een nieuwe model-turn starten, met normale providerkosten.

- **Join** meldt aan, **Leave** trekt in. Na een herstart of reconnect staat ontvangst op pauze tot je **Resume** kiest.
- Een Conversation zonder draaiende agent en zonder open tab verlaat het netwerk vanzelf na 24 uur zonder activiteit. Nog niet afgeleverde berichten aan die agent sluiten met reden `inactive`; opnieuw meedoen kan met **Join**.
- Berichten komen binnen op het eerstvolgende veilige moment en zijn herkenbaar als *peer-origin*, niet als jouw prompt. Ze beantwoorden nooit een toestemmingsvraag en raken je Draft niet.
- Het netwerk deelt geen geschiedenis, concepten of rechten, en biedt geen bestandsvergrendeling.

| Status | Betekenis |
|---|---|
| Queued | Opgeslagen, nog niet aangeboden (agent offline, gepauzeerd of bezig) |
| Delivering | Aangeboden, wacht op bevestiging |
| Delivered | Geaccepteerd door de agent; **geen** leesbevestiging |
| Failed | Expliciet geweigerd; de reden staat erbij |
| Unknown | Niet vast te stellen, bijvoorbeeld na verbindingsverlies |
| Closed | Mag niet meer worden afgeleverd, bijvoorbeeld na Leave |

Failed en Unknown worden nooit vanzelf opnieuw verstuurd.

### Browser en artifacts

**Open browser** opent een Servo-tab; weblinks in een Conversation ook. `Cmd-L` focust de adresbalk, `Cmd-R` herlaadt. Agents krijgen drie tools:

- `show_artifact` toont een HTML-bestand als interactieve pagina in de Conversation of boven de composer. Zo stelt een agent je vragen: een formulier op de pagina stuurt je antwoord terug naar de agent.
- `complete_artifact` sluit zo'n vraag af zodra de agent weet wat hij moet weten. Tot dan blijft de pagina open, zodat je een antwoord kunt verbeteren en opnieuw versturen. Daarna klapt hij in met **Answered** en verlaat hij de plek boven de composer; `Cmd-Shift-A` klapt hem weer uit.
- `browser` opent en bedient een pagina: observeren, klikken, typen, scrollen, wachten op een voorwaarde, screenshots en console- en netwerkfouten. Handig om een agent zijn eigen webapp te laten testen.

> ⚠️ **Geen sandbox.** Pagina's draaien JavaScript en netwerkverzoeken met de rechten van de app. Servo draait in hetzelfde proces: een crash daar sluit de hele app. Geen WebGL, downloads of extensies; maximaal 16 pagina's tegelijk.

### Azure DevOps

Staat de `origin` van een project op Azure DevOps en vindt DROSTJE. de `azdo` CLI, dan wordt elke twee minuten gekeken of er een pull request is vanaf de branch van een worktree-Conversation. Zo ja, dan koppelt DROSTJE. hem: de threadlijst toont `!<PR>` en de work items, en de zijbalk krijgt de tab **Review**. Wordt de pull request voltooid of afgebroken, dan wordt de Conversation één keer gearchiveerd. Zonder `azdo` of `az login` toont Settings → Diagnostics wat er ontbreekt.

### Agents laten doorwerken (macOS)

Zolang een turn loopt of een Factory-run draait, voorkomt DROSTJE. automatische sluimerstand; het scherm mag wel uit. Vergrendel met **Control-Cmd-Q** en laat de app open. **Shift-Cmd-Q logt uit** en stopt je agents. De klep dichtdoen of handmatig laten sluimeren wordt niet tegengehouden.

## Conversations op een Host

Een Host is een vooraf ingerichte machine waarop Conversations draaien. DROSTJE. blijft de regisseur; de Host bezit zijn processen en checkout. Beide verbinden uitgaand via WebSocket met relay **SEINTJE.**; er is geen SSH of bestandssynchronisatie. Factory blijft lokaal.

1. **Tokens.** Maak in SEINTJE twee machine-tokens: één voor de Host, één voor DROSTJE. Configureer SEINTJE met de token-**ID's** (niet de `tk_…`-geheimen):

   ```sh
   RELAY_HOSTS='[{"host":"build-linux","hostToken":"HOST_TOKEN_ID","desktopToken":"DESKTOP_TOKEN_ID"}]'
   RELAY_FRAME_LIMIT=16M
   RELAY_QUEUE_LIMIT=64
   ```

   De ID's vind je met `SELECT id, label FROM tokens` in SEINTJE's database. De reverse proxy moet WebSocket-upgrades doorlaten. Gebruik `wss://`; `ws://` alleen op loopback.

2. **Host configureren.** Maak `host.json` op de Host:

   ```json
   {
     "relay": {
       "id": "build-linux",
       "name": "Linux build machine",
       "relay_url": "wss://seintje.example.org/v1/relay/build-linux",
       "token_env": "DROSTJE_HOST_TOKEN",
       "projects": {}
     },
     "data_dir": "/data",
     "projects": {"app": "/projects/app"},
     "executables": {"pi": "/usr/local/bin/pi"}
   }
   ```

   `projects` koppelt een sleutel aan een absolute map op de Host. De desktop stuurt alleen die sleutel, nooit een eigen pad.

3. **Host starten.** De image bevat Node.js en Pi, maar geen providercredentials; die mount of configureer je zelf.

   ```sh
   docker build -f Dockerfile.host -t drostje-host .
   docker volume create drostje-host-data
   docker run --rm --name drostje-host --env DROSTJE_HOST_TOKEN \
     --mount type=bind,src="$PWD/host.json",dst=/config/host.json,readonly \
     --mount type=bind,src=/absolute/checkout,dst=/projects/app \
     --mount source=drostje-host-data,target=/data \
     drostje-host
   ```

   De container draait als UID 1000; die moet de checkout en `/data` kunnen schrijven. Zonder Docker: `cargo run --no-default-features --features remote-host --bin drostje-host -- /absolute/host.json`.

4. **Desktop koppelen.** Zet het desktop-token in een omgevingsvariabele, bijvoorbeeld `DROSTJE_DESKTOP_TOKEN`, en herstart DROSTJE. Vul in **Settings → Hosts** de Host-ID, relay-URL, naam van die variabele en de project key (`app`) in. Alleen de variabelenaam wordt opgeslagen.

5. **Gebruiken.** Kies in een nieuwe Conversation **Switch execution Host…**, vóór de eerste turn. Changes en bestandsweergaven lezen dan de Host-checkout, alleen-lezen. Lokale terminal-, branch- en commitacties raken de Host niet.

Valt de verbinding weg, dan maakt een lopende turn zijn werk af en bewaart de Host de events. Na een herstart van het Host-proces kies je zelf **Reconnect conversation**; een onbekende turn wordt nooit opnieuw gestart. Bewaar `/data`.

> **Vertrouwensgrens:** TLS beschermt beide verbindingen, maar SEINTJE kan de inhoud lezen; dit is geen end-to-endversleuteling. Gebruik vertrouwde machines en aparte tokens per Host.

## Uiterlijk

Settings → Appearance kiest **System**, **Light** of **Dark**, met een eigen thema per modus:

- **Delft** (standaard): IBM Plex Sans en Lilex, Delfts blauw als accent.
- **Ink**: leesmodus naar e-readers, Libron en Monaspace Xenon, gedempte kleuren.
- **Cootje**: speels, Dosis en Maple Mono, rondere hoeken, Cootje-blauw en saffraan.
- GPUI Kit Default Light en Dark.

**Customize theme…** stelt accentkleur en papierkorrel in, met directe preview. De prompteditor volgt het thema volledig.

Eigen thema's zet je in `<datamap>/themes` als GPUI Kit `ThemeSet`-JSON (een object met een `themes`-array; elk thema een unieke `name` en `mode`). Dit is niet het losse Zed-formaat. Wijzigingen worden direct geladen. De meegeleverde lettertypes vallen onder de SIL Open Font License; zie `assets/fonts`.

## Problemen oplossen

| Probleem | Oplossing |
|---|---|
| Composer toont een Neovim-fout | Installeer Neovim 0.12+ of zet het pad in Settings → Tools. Daarna **Restart Neovim editor**. |
| Een agent ontbreekt in de keuzelijst | Installeer en log in met die agent zelf; zet zo nodig het pad in Settings → Tools. |
| Changes blijft leeg | Is de map een Git-repo? Kies **Refresh conversation changes** in de palette. |
| Een gewijzigde regel opent niet in Neovim | Diff-paden zijn relatief aan de root van de repo. Staat je project in een submap van een repo, open het bestand dan via je eigen Neovim. |
| Factory-run start niet | **Check readiness** in de Factory thread toont de `factory doctor`-uitvoer. |
| Geen pull-requestinfo | Controleer `azdo` en `az login`; Settings → Diagnostics toont wat ontbreekt. |
| Agentnetwerk levert niet af | Na een herstart staat ontvangst op pauze: kies **Resume** in de tab Agents. |
| *Another DROSTJE. instance owns …* | Er draait al een DROSTJE. op dezelfde datamap. Sluit die, of gebruik `DROSTJE_DATA_DIR`. |

## Ontwikkelen

Lees eerst [`AGENTS.md`](AGENTS.md): definition of done en de regels die overal gelden. Verder: [`CODING_STANDARDS.md`](CODING_STANDARDS.md) voor lagen, Rust-conventies en testniveaus, [`docs/agents/ui.md`](docs/agents/ui.md) voor UI en toetsenbord, en [`CONTEXT.md`](CONTEXT.md) voor de vaste termen.

```sh
cargo check --all-targets          # na elke stap
cargo test                         # geen provider-calls
cargo fmt                          # op aangeraakte bestanden
cargo clippy --all-targets         # geen nieuwe warnings
cargo run                          # met DROSTJE_DATA_DIR voor een losse datamap
```

Overige testsets, allemaal zonder betaalde provider-turns:

```sh
# Host-binary
cargo check --no-default-features --features remote-host --bin drostje-host
cargo test --no-default-features --features remote-host --lib remote::

# Terminal-bridge en procesdetectie
cargo test -p ghostty-terminal

# Neovim, rendering en browser
cargo test --test neovim_runtime -- --ignored --nocapture
cargo test --features runtime-qa --test markdown_rendering --test job_notices
cargo test --features runtime-qa --test servo_rendering --test servo_workspace
node --test assets/browser-tools/peer.test.mjs
```

**Desktop-QA** (`runtime-qa`) opent echte vensters en processen op een wegwerp-repo. Maak die eerst aan:

```sh
FX=$(mktemp -d) && mkdir "$FX/target-repo" && cd "$FX/target-repo" \
  && git init -q && printf 'before\n' > notes.txt \
  && git add notes.txt && git commit -qm init && cd -
export DROSTJE_RUNTIME_FIXTURE="$FX"
cargo test --features runtime-qa --lib <testnaam> -- --ignored --nocapture
```

De scenario's staan als `#[ignore]`-tests in `src/ui/runtime_qa.rs`, `src/ui/*_runtime_qa.rs` en `tests/`; de `ignore`-reden noemt wat elk nodig heeft. Een paar scenario's vragen extra omgevingsvariabelen (zoals `DROSTJE_NETWORK_QA_ISOLATED`) en weigeren te draaien zonder. `DROSTJE_QA_SCREENSHOTS=<map>` bewaart screenshots waar een test die maakt.

**Bouwnotities.** GPUI Kit staat vast op `=0.6.6`. Servo 0.7.0 wordt direct in de app gelinkt en draait op een eigen worker-thread. `vendor/` bevat twee kleine patches: `zed-font-kit` (FreeType-versies) en `stylo_derive` (een type-inferentieconflict met GPUI). `Cargo.lock` pint `pkcs1 0.8.0-rc.4` voor Servo's `rsa`; gebruik altijd de lockfile.

---

*Koffie erbij? Dan gaan we.*
