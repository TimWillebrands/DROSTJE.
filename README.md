![DROSTJE. — het stuurhuis voor lokale coding agents](assets/drostje-readme-header.webp)

# DROSTJE.

*Het stuurhuis voor agents op je eigen machines.*

Zet coding agents aan het werk op je eigen repo’s. Schrijf je prompts in Neovim, volg wat er gebeurt en bekijk de wijzigingen naast het gesprek. Voor grotere klussen start je een run met de Compliance Software Factory.

DROSTJE. werkt met OMP, Pi, Claude Code, Codex en OpenCode. Je gebruikt je eigen tools, accounts en rechten, op macOS of Linux.

Een **drost** hield vroeger de boel draaiende als de baas er even niet was. Hier bepaal jij wat er moet gebeuren en houd je zicht op het werk.

[Aan de slag](#aan-de-slag) · [Je werkplek](#je-werkplek) · [Sneltoetsen](#sneltoetsen) · [Samenwerken](#samenwerken) · [Telefoon](#meekijken-op-je-telefoon) · [Hulp](#als-iets-niet-lukt) · [Ontwikkelen](#ontwikkelen)

## Aan de slag

Je hebt [Neovim](https://neovim.io) 0.12+, Git en minstens één ingelogde coding agent nodig. Voor Factory-runs heb je ook de Factory CLI, Docker en de agent-image nodig.

De desktop-app bouw je voorlopig zelf. Installeer Rust stable, Zig 0.16, CMake, pkg-config, Python 3 en make. Op macOS heb je de Xcode-tools nodig; op Linux Wayland/EGL/OpenGL, fontconfig, libxml2 en libc++ van LLVM 21 of nieuwer.

```sh
./install.sh
```

Op macOS verschijnt **DROSTJE.app** in Applications. Op Linux komt de app onder `~/.local`, met een item in je applicatiemenu. De eerste build duurt even: Ghostty en Servo worden meegebouwd. Daarna wordt de buildcache gebruikt.

De macOS-app is ongesigneerd. Linux is getest op Arch met COSMIC/Wayland; de terminal ondersteunt geen X11.

Ontbreekt er een tool? **Settings → Tools** laat zien wat er nodig is en waar DROSTJE. zoekt. Installeren en inloggen doe je zelf.

### Je eerste Conversation

1. Kies **Add project…** en open een Git-repo.
2. Klik op de **+** naast het project en kies een agent. Met `Cmd-N` start je de standaardagent van het project.
3. Typ je prompt in de composer. Dat is Neovim: Return maakt een nieuwe regel, `Cmd-Return` verstuurt.
4. Volg de antwoorden en toolkaarten. In **Changes** zie je ondertussen wat er in je werkboom verandert.
5. Vraagt de agent om toestemming of invoer, beantwoord dat dan op de kaart in de Conversation.
6. Klaar om iets vast te leggen? **Commit…** boven de diff opent het commitformulier.

`Cmd-.` onderbreekt een turn. Met `Cmd-Shift-P` open je de command palette: daar vind je alle opdrachten en hun sneltoetsen.

## Je werkplek

Links staan je projecten en threads, in het midden je open tabs. Rechts heeft iedere thread een eigen zijbalk met onder andere **Changes**, **Terminal** en **Browser**. Sleep tabs naast of onder elkaar om de indeling aan te passen.

Een **Conversation** is een gesprek met een agent. Een **Thread** is zo’n Conversation of een Factory thread. Met **Spaces** groepeer je projecten en threads per klus; iedere Space onthoudt zijn eigen tabs en indeling.

### Prompts, bestanden en terminals

De composer bewaart je concept, de **Draft**. Neovim werkt zoals je gewend bent, met modi, registers en undo. `:q`, `:wq` of `ZZ` verlaat de editor; je concept blijft staan. Plak afbeeldingen met `Cmd-V` of sleep bestanden in het gesprek.

Concepten worden bewaard als je de editor of een veld verlaat, van thread wisselt of iets bevestigt. Tijdens het typen is er geen opslagtimer. Bij een crash kun je daardoor de laatste onopgeslagen tekst verliezen.

Open een bestand door op een pad in het gesprek te klikken, of gebruik `Cmd-Alt-O`. Het opent in een Neovim-tab met je eigen config en plugins. De ingebouwde Ghostty-terminal gebruikt je eigen shell.

In **Changes** bekijk je de diff inline of naast elkaar. Return opent de geselecteerde regel in Neovim; `s` wisselt de weergave. Terminaltabs blijven draaien als je van thread wisselt, maar beginnen na een app-herstart opnieuw.

### Agents en worktrees

Je kiest de agent per Conversation. Het model, de effort en de toestemmingsmodus staan onderaan de composer; de beschikbare keuzes verschillen per agent. Bij heropenen hervat de agent zijn eigen sessie.

Wil je agents tegelijk laten werken? **New conversation in new worktree…** (`Cmd-Alt-N`) geeft een Conversation een eigen Git-werkboom en branch. Met `Cmd-N` vanuit die Conversation deel je dezelfde worktree.

Onder de composer zie je de werkruimte en branch. Daar kun je ook van branch wisselen, zolang er geen turn loopt en de werkboom schoon is. Een worktree wordt pas vanzelf opgeruimd als alle threads gearchiveerd zijn en hij schoon en gemerged is.

### Factory

Een Factory thread start een run voor een Azure DevOps User Story of een PRD/issues-map. **Check readiness** controleert of alles klaarstaat; **Start run** start het werk. Daarna volg je de events en resultaten in de thread.

Een nieuwe poging begint in een nieuwe Factory thread. Factory-processen draaien door als je DROSTJE. sluit. Ontbreekt een afsluitend resultaat, dan blijft de uitkomst **Unknown**; exitcode 0 alleen is geen bewijs dat de run geslaagd is.

### Je eigen indeling en uiterlijk

Wissel van Space via de kiezer linksboven, `Cmd-Alt-]` / `Cmd-Alt-[` of een horizontale tweevingerveeg boven Projects. Lopende agents werken verder. Threads en projecten kun je via **Move … to space…** verplaatsen.

In **Settings → Appearance** kies je System, Light of Dark en een thema: Delft, Ink, Cootje of GPUI Kit Default. **Customize theme…** past accentkleur en papierkorrel aan, met directe preview. De prompteditor volgt het thema mee.

Eigen thema’s kun je als GPUI Kit ThemeSet-JSON in de map `themes` van je datamap zetten. Wijzigingen worden direct geladen. De lettertypes en hun licenties staan in [assets/fonts](assets/fonts).

## Sneltoetsen

`Cmd` betekent Command op macOS en Control op Linux; `Alt` is Option. Op Linux gebruik je `Ctrl-Shift-W` om een tab te sluiten en `Ctrl-Shift-V` om te plakken, zodat de gebruikelijke editortoetsen vrij blijven.

| Toets | Wat je ermee doet |
|---|---|
| `Cmd-Shift-P` | Command palette openen |
| `Cmd-N` / `Cmd-Shift-N` | Nieuwe Conversation / Factory thread |
| `Cmd-Alt-N` | Nieuwe Conversation in een eigen worktree |
| `Cmd-Return` / `Cmd-.` | Prompt versturen / turn onderbreken |
| `Cmd-Alt-T` / `Cmd-Alt-O` | Terminal / bestand in Neovim openen |
| `Cmd-B` / `Cmd-J` | Projecten / zijbalk tonen of verbergen |
| `Cmd-Alt-h/j/k/l` | Naar het paneel in die richting |
| `Ctrl-Tab` / `Ctrl-Shift-Tab` | Volgende / vorige tab |
| `Cmd-W` | Tab sluiten; de thread blijft bewaard |
| `Cmd-,` | Settings openen |

In lijsten werken `j`/`k` en de pijltjes, `gg`/`G` gaat naar begin of eind en Return opent je keuze. In Projects klappen `h`/`l` een project in en uit; `/` zoekt. In de Conversation spring je met `i` naar de composer.

## Samenwerken

### Agents onderling

Via **Join** in de tab **Agents** laat je een Conversation meedoen aan het netwerk van zijn Space. Agents kunnen elkaar dan vinden en berichten sturen. Met **Leave** trek je dat weer in.

Een bericht kan een wachtende agent aan het werk zetten en dus providerkosten maken. Na een app-herstart of reconnect staat ontvangst op pauze tot je **Resume** kiest.

Berichten delen geen geschiedenis, concepten of rechten. **Delivered** betekent dat de agent het bericht heeft geaccepteerd, niet dat hij het al heeft gelezen of uitgevoerd. **Failed** en **Unknown** worden nooit vanzelf opnieuw verstuurd.

### Browser en interactieve pagina’s

De ingebouwde browser opent websites naast je Conversation. Agents kunnen pagina’s bedienen, screenshots maken en hun eigen webapp testen. Met `Cmd-L` ga je naar de adresbalk; `Cmd-R` herlaadt.

Een agent kan ook een interactieve pagina in het gesprek tonen, bijvoorbeeld om opties te vergelijken of je iets te laten invullen. Je kunt je antwoord aanpassen zolang de pagina openstaat. Na afronden klapt hij in met **Answered**.

Browserpagina’s hebben JavaScript en netwerktoegang met de rechten van de app. Servo draait in hetzelfde proces; een browsercrash kan dus de hele app sluiten. WebGL, downloads en extensies worden niet ondersteund.

### Azure DevOps

Voor een project met Azure DevOps als origin kan DROSTJE. de pull request bij een worktree-Conversation vinden. De zijbalk krijgt dan een tab **Review**. Hiervoor heb je de `azdo` CLI en een actieve Azure-login nodig.

### Conversations op een Host

Een **Host** is een andere machine waarop je agent draait, bijvoorbeeld een Linux-buildmachine. Je volgt de Conversation vanuit DROSTJE. via de relay **SEINTJE.** De checkout en processen blijven op de Host; Factory blijft lokaal.

Richt de Host in met de [voorbeeldconfiguratie](examples/host/host.json), het [Compose-bestand](examples/host/compose.yml) en de [relay-instellingen](examples/host/relay.env.example). Pas de adressen en projectpaden aan je eigen machines aan.

Maak in SEINTJE aparte machine-tokens voor Host en desktop. De relayconfiguratie gebruikt hun ID’s; de clients gebruiken de tokens zelf. Laat je reverse proxy WebSocket-verbindingen door en gebruik een beveiligde verbinding.

De container bevat Node.js en Pi. Providercredentials voeg je zelf toe; de container draait als UID 1000 en moet bij de checkout en zijn datavolume kunnen. Bewaar dat volume, ook als je de container vervangt.

Koppel de machine in **Settings → Hosts**. Kies daarna **Switch execution Host…** in een nieuwe Conversation, vóór de eerste turn. Changes en bestanden lezen de Host-checkout; lokale terminal-, branch- en commitacties blijven lokaal.

Bij verbindingsverlies maakt een lopende turn zijn werk af. Na een Host-herstart kies je zelf **Reconnect conversation**. SEINTJE kan de doorgestuurde inhoud lezen; gebruik hiervoor vertrouwde machines en aparte tokens.

## Meekijken op je telefoon

Met de Android-app volg je Spaces, Threads en de geopende Conversation, ook onderweg. Prompts sturen en vragen beantwoorden doe je op de computer. De app is er voor arm64-telefoons met Android 11 of nieuwer.

1. Vul in **Settings → Phones** een ntfy-server in, bijvoorbeeld je SEINTJE of `https://ntfy.sh`. Heeft die een token nodig, dan geef je de naam van de omgevingsvariabele op.
2. Kies **Pair phone…** op de computer en **Scan code** in de app. Scan de QR-code en vergelijk het getal van zes cijfers. Klopt het, kies dan **Allow phone**. De code is vijf minuten geldig.
3. Open een Conversation op je telefoon om live mee te lezen. Als DROSTJE. uitstaat, zie je de laatst bekende stand. Oudere berichten blijven op de computer.

De app neemt het thema van de computer over: Delft, Ink, Cootje of je eigen thema, met accentkleur en papierkorrel. Kies je in **Settings → Appearance** voor System, dan volgt de telefoon zijn eigen lichte of donkere stand. Zolang de telefoon nog niet gekoppeld is, gebruikt hij Delft.

Ontkoppelen kan op de telefoon via **Unpair this phone** of op de computer via **Settings → Phones → Remove phone…**. Berichten zijn end-to-end versleuteld; de server ziet alleen versleutelde gegevens en willekeurige topicnamen.

## Als iets niet lukt

| Wat je tegenkomt | Waar je kunt kijken |
|---|---|
| Neovim-fout in de composer | Controleer Neovim 0.12+ in Settings → Tools, kies daarna **Restart Neovim editor**. **Copy draft recovery** helpt je concept terug te halen. |
| Agent ontbreekt | Installeer de agent, log ermee in en controleer het pad in Settings → Tools. |
| Changes blijft leeg | Controleer of de map een Git-repo is en kies **Refresh conversation changes** in de palette. |
| Factory-run start niet | **Check readiness** toont wat er ontbreekt. |
| Geen pull-requestinfo | Controleer je Azure-login en Settings → Diagnostics. |
| Agentnetwerk levert niet af | Kies na een herstart **Resume** in de tab Agents. |
| Er draait al een instantie | Sluit de andere DROSTJE. die dezelfde datamap gebruikt. |

Op macOS houdt DROSTJE. je machine wakker zolang een turn of Factory-run loopt. Je kunt het scherm vergrendelen met `Control-Cmd-Q`. Uitloggen, de klep sluiten of handmatig laten sluimeren stopt dat doorwerken wel.

Je gegevens staan op macOS in `~/Library/Application Support/Drostje` en op Linux standaard in `~/.local/share/drostje`. Projecten, threads, concepten en instellingen worden lokaal bewaard. Archiveren verwijdert niets.

## Ontwikkelen

Begin bij [AGENTS.md](AGENTS.md) voor de werkafspraken, [CODING_STANDARDS.md](CODING_STANDARDS.md) voor de code en [CONTEXT.md](CONTEXT.md) voor de termen. Voor UI-werk is er de [UI-handleiding](docs/agents/ui.md).

De desktop-app staat in `src/`; de afzonderlijke onderdelen hebben een eigen crate onder `crates/`. De Android-app staat in `mobile/`. Native patches staan in `vendor/`, met uitleg in [vendor/README.md](vendor/README.md).

```sh
cargo run
cargo check --all-targets
cargo test
cargo clippy --all-targets
```

Formatteer alleen aangeraakte Rust-bestanden met `rustfmt --edition 2024`. Tests doen geen betaalde provider-calls. Voor een aparte testinstallatie kun je met `DROSTJE_DATA_DIR` een andere datamap kiezen.

### Android bouwen en uitbrengen

`bash mobile/build.sh` bouwt een debug-APK. Benodigdheden, bestandspaden en de eenmalige signingconfiguratie staan bij het script:

```sh
bash mobile/build.sh --help
```

De [release-workflow](.github/workflows/android-release.yml) maakt bij een tag zoals `android-v0.1.0` een ondertekende APK met controlesom en release notes. Een tag zoals `android-v0.1.0-rc.1` maakt een prerelease.

<details>
<summary>Extra tests en desktop-QA</summary>

Host, terminal en browser-tools:

```sh
cargo check -p drostje-remote --bin drostje-host
cargo test -p drostje-remote
cargo test -p ghostty-terminal
node --test assets/browser-tools/peer.test.mjs
```

Neovim, rendering en browser:

```sh
cargo test -p drostje-neovim --test neovim_runtime -- --ignored --nocapture
cargo test --features runtime-qa --test markdown_rendering --test job_notices
cargo test --features runtime-qa --test servo_rendering --test servo_workspace
```

Desktop-QA opent echte vensters en processen. Gebruik daarvoor altijd een tijdelijke repo:

```sh
FX=$(mktemp -d) && mkdir "$FX/target-repo" && cd "$FX/target-repo" \
  && git init -q && printf 'before\n' > notes.txt \
  && git add notes.txt && git commit -qm init && cd -
export DROSTJE_RUNTIME_FIXTURE="$FX"
export RUST_MIN_STACK=8388608
cargo test -p drostje --features runtime-qa --lib <testnaam> -- --ignored --nocapture --test-threads=1
```

De scenario’s staan als genegeerde tests in `src/ui/runtime_qa/` en `tests/`. Hun `ignore`-reden vermeldt eventuele extra vereisten. De grotere stack hierboven is nodig voor diepe debug-scenario’s.

Android-schermtests op macOS:

```sh
cd mobile
DROSTJE_PHONE_SHOTS=/tmp/shots cargo test --test screens
```

Dit schrijft per toestand een licht en donker beeld. Met `DROSTJE_QA_SCREENSHOTS` kun je ook desktoptests die screenshots ondersteunen een uitvoermap geven.

</details>
