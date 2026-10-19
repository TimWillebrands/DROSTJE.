![DROSTJE. — Cootje stuurt lokale coding agents en de Factory aan](assets/drostje-readme-header.webp)

# DROSTJE.

*Het stuurhuis voor de agents op je machine.*

DROSTJE. is een bureaublad-app waarmee je de coding agents op je eigen computer aanstuurt. Je zet ze aan het werk, kijkt mee terwijl ze bezig zijn, en stuurt grotere klussen door naar de Factory. Alles op één plek, en alles blijft van jou: je eigen repo's, je eigen tools, je eigen accounts.

## Waarom "drost"?

Een **drost** (of drossaard, als je het deftig wilt zeggen) was vroeger de rentmeester van de heer. Degene die de boel draaiende hield als de baas er even niet was. Precies dat doet DROSTJE. voor jou: jij zegt wat er moet gebeuren, DROSTJE. zorgt dat het gebeurt en houdt in de gaten hoe het loopt.

## Wat je ermee doet

- 🗂️ **Je projecten bij elkaar houden.** Gewoon je bestaande git-repo's, geen gedoe.
- **Werk scheiden met Spaces.** Groepeer projecten en hun threads, met eigen open tabs, selectie en paneelindeling per Space.
- **Meerdere threads openhouden.** Conversations en Factory-threads staan naast elkaar in tabs; de open tabs worden bewaard.
- **Een echte terminal naast je threads.** Open een libghostty-terminal in de projectmap, als eigen tab of gesplitst paneel. Shellsessies blijven alleen tijdens deze app-sessie bestaan.
- **Prompts met Neovim schrijven.** Echte normal-, insert- en visual-modi, operators, registers en undo/redo, ingebed in de bestaande composer zonder regelnummers of terminalvenster.
- **Factory-runs starten.** Een bestaande User Story of PRD/issues-map kiezen, launch-parameters instellen en daarna de native events en resultaten volgen.
- 📜 **Terugzien wat er gebeurd is.** Elke run blijft bewaard, ook als je DROSTJE. tussendoor even dichtdoet.

## Wat DROSTJE. níet doet

- **Niks verzinnen.** Mislukt een run, dan zie je dat. Is iets onbekend, dan staat er "onbekend".
- **Niks stiekem installeren of bijwerken.** DROSTJE. doet dat alleen als jij er expliciet om vraagt en logt nooit voor je in. Ontbreekt er iets, dan zie je dat in Settings.
- **Geen editor voor je hele codebase.** Daar heb je andere dingen voor.
- **Niet zelf opnieuw proberen.** Een drost doet wat je vraagt, niet meer en niet minder.

## Aan de slag

```sh
cargo build --manifest-path crates/servo-browser/Cargo.toml --locked
cargo run
```

Wat ontbreekt, vertelt DROSTJE. je zelf. Wil je een aparte data-map (handig voor testen)? Zet `DROSTJE_DATA_DIR`.

Een bronbuild vereist **Zig 0.16** (`zig` op PATH, of `ZIG=/pad/naar/zig`) voor de ingebedde libghostty-adapter. Op macOS is ook Xcode nodig; Linux vereist Wayland/EGL/OpenGL, libxml2 en de libc++-runtime van LLVM 21 of nieuwer. De eerste build compileert de meegeleverde Ghostty-broncode; volgende builds hergebruiken een cache op basis van broncode en toolchain. `GHOSTTY_NATIVE_CACHE_DIR` kan die cache verplaatsen. Een gebouwde app heeft geen Zig of afzonderlijk geïnstalleerde Ghostty nodig.

De ingebedde browser gebruikt **Servo 0.7.0** in `drostje-servo`. Die helper heeft een eigen Cargo-workspace omdat Servo en de vastgepinde GPUI-versie verschillende `freetype-sys`-versies linken. Nodig: CMake, pkg-config, Python 3, make en de platform-SDK; Linux heeft daarnaast fontconfig-ontwikkelbestanden en Mesa EGL/OpenGL nodig. SpiderMonkey downloadt normaal een passend vooraf gebouwd archief. Een bronbuild daarvan vereist LLVM/Clang 19 of nieuwer; Apple Clang 17 volstaat niet. De installer bouwt en bundelt de helper; een ontwikkelbuild vindt hem onder `crates/servo-browser/target/{debug,release}`. Met `DROSTJE_SERVO` kun je een expliciet executable-pad opgeven. Ontbreekt de helper, dan blijft de app bruikbaar en toont het browserpaneel de bouwinstructie.

### Installeren

Eén script bouwt een release-build en installeert die voor je desktop:

```sh
./install.sh
```

Op **macOS** maakt de installer een ongesigneerde `DROSTJE.app`-bundle (weergegeven als DROSTJE.) in `/Applications` en vraagt alleen bij een niet-schrijfbare map om beheerdersrechten. Gebruik `INSTALL_DIR` voor een andere locatie:

```sh
INSTALL_DIR="$HOME/Applications" ./install.sh
```

Op **Linux** (getest op Arch met COSMIC/Wayland) is `INSTALL_DIR` een prefix, standaard `~/.local`: het binaire bestand komt in `bin/`, een `drostje.desktop`-item en de hicolor-iconen in `share/`. Daarna staat DROSTJE. in je app-launcher; ImageMagick (`magick`) is optioneel voor de geschaalde iconen. Voor een systeembrede installatie:

```sh
INSTALL_DIR=/usr/local ./install.sh
```

De app-installer installeert geen Neovim, OMP, Pi, Claude Code, Factory, Docker of andere vereisten. Ontbrekende vereisten configureer je in Settings. Als Pi ontbreekt, kan **Install Pi** daar na een expliciete klik de officiële `@earendil-works/pi-coding-agent`-package zonder lifecycle-scripts in de DROSTJE.-datamap installeren; daarvoor zijn Node.js 22.19 of nieuwer en npm nodig.

De standaardmap is `~/Library/Application Support/Drostje` op macOS en `$XDG_DATA_HOME/drostje` (standaard `~/.local/share/drostje`) op Linux. Eén instantie mag die map tegelijk gebruiken. Projecten, threads, concepten, instellingen en launch-verwijzingen staan in SQLite; sessies, bijlagen en logs blijven bestanden. Archiveren verwijdert niets. Deze nieuwe versie gebruikt schema 2 zonder migraties; een niet-ondersteunde database wordt geweigerd, niet vervangen.

Kies in Settings zo nodig expliciete executable-paden, de Conversation-harness per project en de gedeelde Factory-data-root. OMP, Pi en Claude Code draaien met je eigen lokale rechten; Factory beheert zijn eigen containers en Git-worktrees. Sluiten stopt interactieve sessies, maar niet reeds gestarte Factory-processen. Een onbekende uitkomst wordt nooit automatisch opnieuw verstuurd.

### Agents laten doorwerken op macOS

DROSTJE. voorkomt automatische sluimerstand zolang een Conversation-turn actief is of een door de app gestart Factory-proces draait. Het scherm mag wel uitgaan of vergrendelen. Zodra het werk klaar is, vervalt de bescherming; alleen een Conversation open laten staan houdt de Mac niet wakker. Er worden geen systeeminstellingen gewijzigd.

Gebruik **Control-Cmd-Q** om het scherm te vergrendelen en laat DROSTJE. open. **Shift-Cmd-Q logt uit** en is dus niet geschikt om je agents te laten doorwerken. Expliciete sluimerstand, het sluiten van de MacBook-klep en sluimerstand door een lege accu worden niet tegengehouden. Bij het afsluiten van DROSTJE. vervalt ook de slaapbescherming voor nog draaiende Factory-processen.

### Spaces

Een **Space** is een eigen werkcontext voor een groep projecten, geïnspireerd door [Zen](https://docs.zen-browser.app/user-manual/workspaces). De Space-kiezer in Projects wisselt de hele context; de `+` ernaast maakt een lege Space. Bestaande projecten en open threads komen bij de eerste start in **Default** terecht.

Elke Space bewaart zijn eigen open tabs, geselecteerde project en thread, ingeklapte projecten, archiefweergave en indeling van threadpanelen. Projects blijft één gedeelde zijbalk: breedte en open/dicht-toestand blijven gelijk bij het wisselen en na een herstart. Projects, zoeken en de project- en threadresultaten in de command palette tonen alleen de actieve Space. Wisselen stopt geen lopende agent of Factory-run en bewaart concepten. Onvoltooide Neovim-opdrachten of tekstcompositie moeten eerst worden afgerond of geannuleerd.

Via het Space-menu of de command palette kun je een Space hernoemen, een project vanuit de actieve Space verplaatsen, of een Space verwijderen. Een project kan in meerdere Spaces staan: voeg dezelfde Git-werkboom in een andere Space opnieuw toe om hem daar ook te gebruiken. Elke Conversation- of Factory-thread hoort bij precies één combinatie van project en Space; een nieuwe thread blijft dus alleen zichtbaar in de Space waar je hem maakt. Verplaats één thread via **Move thread to space** in het contextmenu van die thread; het project wordt zo nodig automatisch aan de doel-Space toegevoegd. **Move project to space…** verplaatst het project en alleen zijn threads uit de actieve Space, zonder lidmaatschappen of threads in andere Spaces te wijzigen. Bij het verwijderen van een Space gaan de threads en projecten zonder andere plek naar Default. Historie, concepten en bestanden blijven behouden. Default zelf kan niet worden verwijderd.

Gebruik **Switch space…** voor de toetsenbordkiezer: `j/k`, pijlen, `gg/G`, Return en Escape. **Next space** en **Previous space** wisselen direct buiten invoervelden en overlays. Spaces scheiden organisatie, niet accounts, credentials of bestandstoegang; tools en thema-instellingen blijven gedeeld.

Een horizontale tweevingerveeg boven Projects schuift de project- en threadlijst rechtstreeks mee met je vingers; de lijst van de aangrenzende Space komt tegelijk in beeld. Je kunt pauzeren of terugvegen zonder al te wisselen. Pas bij loslaten wisselt een voldoende lange veeg één Space; een korte of teruggedraaide veeg keert terug, zonder extra wissels door uitrollen. Op platforms zonder aanraakfasen geldt een korte pauze als loslaten. De zijbalkrand, zoekbalk, Space-kiezer, werkbalk en centrale werkruimte blijven op hun plaats. Wisselen via toetsenbord of kiezer krijgt dezelfde korte schuifanimatie. Bij verminderde beweging blijft de wissel bij loslaten direct, zonder animatie.

### Threads en werkruimte

Projects bevat binnen de actieve Space een lijst met twee soorten threads:

- **Conversation:** een interactieve OMP- of Claude Code-sessie met geschiedenis, berichtballonnen, bijlagen met previews en een ingebedde Neovim-prompteditor met GPUI Kit-styling. Klik op de `+` naast een project en kies eerst **OMP** of **Claude Code**. Dezelfde keuzes staan onder **New thread** en in de command palette; `Cmd-N` gebruikt de gekozen projectstandaard. Rechts toont een inklapbare zijbalk de actuele Git-wijzigingen terwijl een turn loopt; `Toggle conversation changes` en `Refresh conversation changes` staan ook in de command palette. Het model kies je onderaan de composer. Met **New conversation in new worktree…** (`Cmd-Alt-N`) start een Conversation in een eigen Git-worktree onder de app-datamap, op een nieuwe branch `drostje/<naam>` vanaf een gekozen lokale basisbranch; na de eerste gegenereerde titel krijgt die branch zijn definitieve naam. `Cmd-N` vanuit zo'n Conversation deelt dezelfde worktree; de composer en de threadlijst tonen codenaam en branch. Is de laatste thread van een worktree gearchiveerd en is de worktree schoon en in zijn basisbranch gemerged, dan wordt hij automatisch verwijderd; anders blijft hij staan tot **Remove worktree…**. **Switch branch…** wisselt de branch van de werkboom van de geselecteerde Conversation, of maakt een nieuwe branch vanaf HEAD; dat kan niet tijdens een turn en niet met niet-gecommitte wijzigingen. De labels volgen de echte Git-HEAD, ook na een `git switch` in een terminal.
- **Factory:** parameters voor een bestaande Azure DevOps User Story of een bestaande PRD/issues-map. `Advanced` bevat de branch en het optionele thought level. `Start run` slaat de definitieve configuratie eerst op; een opslagfout start niets.

**Archive thread** archiveert direct, zonder bevestigingsvenster. Geschiedenis en concept blijven bewaard en de archiefweergave wordt geopend. Een actieve turn, een lopende Factory-start of een eigen Factory-proces blijft archiveren blokkeren; projecten archiveren vraagt nog wel bevestiging.

**Terminal** is een derde soort centraal paneel, geen opgeslagen thread. Kies **New terminal** in het projectmenu, het tabmenu of de command palette (`Cmd-Alt-T`). De standaard-shell start in de projectmap met je eigen gebruikersrechten. Op macOS is dit een login-shell, zodat systeem- en gebruikersprofielen (zoals `.zprofile` voor zsh) vóór de interactieve configuratie worden geladen, ook wanneer de app vanuit Finder start. Tabs, splitsingen, `Ctrl-Tab` en `Ctrl-W` + `h/j/k/l` werken ook voor terminals. `Cmd-Escape` verlaat de terminalinvoer zonder de shell te stoppen; `Ctrl-C` blijft een terminaltoets.

Een terminal blijft draaien bij tab- en Space-wissels. Sluiten of **Restart terminal** vraagt bevestiging zolang de shell draait en stopt die shell plus het actieve proces op de voorgrond. Na een gewone shell-exit blijft het paneel zichtbaar met een herstartactie. Afsluiten van de app stopt alle terminals na bevestiging; terminaltabs en shellsessies worden niet bewaard of automatisch opnieuw gestart. Opstart- en rendererfouten blijven in het betreffende paneel zichtbaar, zonder Conversations of Factory-threads te blokkeren. De native backend gebruikt Metal op macOS en EGL/OpenGL op Linux/Wayland; X11 wordt voor Terminal niet ondersteund.

### Browser en HTML-artifacts

**Open browser** in de command palette opent een zelfstandig Servo-tabblad. Weblinks in Conversations openen ook zo'n tabblad. **Toggle browser sidebar** toont een blijvende browsersessie naast de Conversation. Sleep de scheidingslijn om de zijbalk te schalen, of gebruik **Widen thread sidebar** / **Narrow thread sidebar** in de palette; de breedte wordt per thread bewaard.

De adresbalk accepteert web-URL's en absolute bestandspaden. `Cmd-L` focust het adres, Return navigeert en focust de pagina, `Cmd-R` herlaadt. Terug/vooruit, muis, scrollen, toetsenbord en tekstinvoer werken op de echte Servo-pagina. `Cmd-Escape` verplaatst focus van de pagina naar de adresbalk. Browser-tabs delen de gewone tabnavigatie en `Close tab`.

OMP, Pi en Claude Code krijgen bij het starten van hun sessie instructies en twee tools:

- `show_artifact`: schrijf eerst een HTML-bestand, bijvoorbeeld `/tmp/chart.html`, en geef daarna `{"path":"/tmp/chart.html","title":"Chart"}` aan de tool. De toolkaart in de Conversation wordt een interactieve browser. Relatieve CSS-, JavaScript- en afbeeldingsbestanden blijven naast het HTML-bestand werken.
- `browser`: open met `{"action":"open","url":"http://localhost:3000","placement":"sidebar"}` of `"placement":"tab"`. Gebruik het teruggegeven `id` voor `inspect`, `navigate`, `back`, `forward`, `reload`, `evaluate`, `click`, `type`, `key`, `scroll`, `screenshot` en `close`. `evaluate` geeft het echte JavaScript-resultaat terug; screenshots zijn PNG-bestanden. Navigatie en invoer worden in de wachtrij geplaatst: controleer `inspect` of de DOM vóór de volgende stap. In Claude Code heten de tools `mcp__drostje__show_artifact` en `mcp__drostje__browser`.

Dit is bewust **geen sandbox**. Pagina's voeren JavaScript en netwerkverzoeken uit met de mogelijkheden van de browser; harness-toestemmingen beperken die pagina's niet. De helper is een link-/procesgrens, geen beveiligingsgrens.

Tab-URL's, artifact-verwijzingen en zijbalkkeuzes blijven bewaard; een herstart laadt pagina's opnieuw, niet hun JavaScript- of formulierstatus. HTML-bestanden worden niet gekopieerd: verwijderde `/tmp`-bestanden geven een zichtbare fout. Servo is geen Chrome-vervanger: deze build gebruikt Servo's `SoftwareRenderingContext` en kopieert RGBA-frames naar GPUI, heeft geen WebGL/WebGPU en ondersteunt maximaal 16 gelijktijdige pagina's. Browserextensies, downloads, uploadkiezers en pop-upvensters zijn niet geïntegreerd. De browser- en werkruimteproeven zijn op macOS uitgevoerd; Linux-rendering is nog niet runtime-gecontroleerd.

De Conversation-tijdlijn toont denkstappen als gedempte inline tekst. Ingeklapte tools houden een begrensd voorbeeld van hun resultaat zichtbaar; open de kaart voor de volledige uitvoer. Mislukte tools openen hun foutdetails automatisch.

Na de eerste duurzame launch wordt de Factory-thread een alleen-lezen monitor met native events, resultaten en ruwe details. Ook een geweigerde launch blijft als historie bewaard. Een volgende poging begint in een **nieuwe Factory-thread**. Er is geen taakpakket-generator, handoff vanuit een bericht of Factory-composer.

De werkruimte heeft Projects links en Conversations, Factory-threads en terminals centraal. Changes en Output horen bij de zijbalk van hun thread. Elk centraal paneel is een eigen dock-paneel: sleep een tab naast of onder een andere om meerdere panelen tegelijk in beeld te hebben, of laat ze als tabs bij elkaar. Je kunt de native docks verplaatsen, groeperen en schalen; threadtabs en hun indeling worden bewaard, terminaltabs alleen tijdens de huidige app-sessie. Het paneel dat focus krijgt (klik, tab, of `Ctrl-W` + `h/j/k/l`) is het geselecteerde paneel; alleen de geselecteerde Conversation host de Neovim-editor, andere zichtbare Conversations tonen hun bewaarde concept met de hint `Press i to edit`. Bij een thread sluit `Close tab` alleen het paneel en eventueel een inactieve Conversation-sessie, niet de historie of een lopende turn of Factory-run. Bij sluiten krijgt een aangrenzende tab focus. Concepttekst blijft behouden bij wisselen, sluiten en opnieuw openen.

Zonder geopende centrale panelen zie je compacte startacties in het midden; een lege tabstrip wordt niet getoond. Opslagstatus staat in de titelbalk, niet in een aparte onderbalk. De zijbalk van een thread blijft bereikbaar via `Cmd-J` en de command palette.

Tijdens het typen worden Conversation-concepten en Factory-velden alleen in het geheugen bijgewerkt: er loopt geen opslagtimer en de melding ‘Saving changes…’ verschijnt niet voor wachtende edits. Opslag begint bij het verlaten van de editor of een veld, het wisselen of sluiten van een thread, of een expliciete bevestiging. Return bevestigt een Factory-veld. Verzenden, Factory Start en normaal afsluiten wachten op de benodigde opslag. Bij een crash kunnen wijzigingen sinds de laatste bevestiging of focuswissel verloren gaan.

### Toetsenbord

`Cmd` is de Command-toets op macOS en Control op Linux. Uitzonderingen op Linux: `Cmd-W` is `Ctrl-Shift-W` (een losse Ctrl-W begint de paneelreeks) en `Cmd-V` is `Ctrl-Shift-V` (Ctrl-V is van Vim).

|Toets|Actie|
|---|---|
|`Cmd-Shift-P`|Command palette: opdrachten, projecten en beide threadsoorten|
|`Cmd-N` / `Cmd-Shift-N`|Nieuwe Conversation / Factory-thread|
|`Cmd-Alt-N`|Nieuwe Conversation in een nieuwe Git-worktree|
|`Cmd-Alt-T`|Nieuwe Terminal in het geselecteerde project|
|`Cmd-1` / `Cmd-2` / `Cmd-3`|Focus Projects / Thread / Output; opent zo nodig het dock|
|`Cmd-B` / `Cmd-J`|Projects / Output tonen of verbergen|
|`Ctrl-Tab` / `Ctrl-Shift-Tab`|Volgende / vorige centrale tab, inclusief terminals|
|`Cmd-Alt-]` / `Cmd-Alt-[`|Volgende / vorige Space, buiten invoervelden en overlays|
|`Cmd-W` (Linux: `Ctrl-Shift-W`)|Huidige tab sluiten; bij een draaiende terminal met bevestiging|
|`Ctrl-W`, daarna `h/j/k/l`|Naar het dichtstbijzijnde zichtbare paneel in die richting, ook vanuit de prompteditor of terminal; zonder buur blijft focus staan, zonder omslaan of diagonale sprongen|
|`j/k`, pijlen, `gg/G`, Home/End, Page Up/Down, `h/l`, Return|Navigeren, springen, bladeren, invouwen/uitvouwen en openen in Projects; in een Conversation scrollen dezelfde toetsen de geschiedenis|
|`i` in een Conversation|Focus naar de prompteditor|
|`Cmd-V` in een Conversation|Afbeelding van het klembord als bijlage toevoegen (PNG, JPEG, WebP, TIFF); tekst plakt in de editor|
|`Cmd-Enter`|Prompt versturen, Factory-configuratie starten of formulier bevestigen|
|`Cmd-.`|Interactieve turn onderbreken|
|`Cmd-Escape`|Prompteditor- of terminalinvoer verlaten|
|`Cmd-,`|Settings|
|Escape|Bovenste menu of venster sluiten; in de palette eerst de zoektekst wissen|

Focus gebruikt subtiele, neutrale tintverschillen in plaats van blauwe omlijningen. In Projects heeft de rij waarop `j/k` en Return werken een gedempte achtergrond en een smalle neutrale markering links; de geopende thread blijft apart herkenbaar. De prompteditor krijgt bij focus een subtiel gekleurde kop met **Draft · Editing** en toont **Draft · Not focused** zodra je naar een ander onderdeel gaat. De concepttekst, cursorpositie en Neovim-modus blijven behouden. Projects bereik je met een paneelbeweging vanuit de aangrenzende thread, niet door vanuit een verder gelegen thread over tussenliggende panelen heen te springen.

In insert-modus voegt Return een nieuwe regel toe; alleen `Cmd-Enter` verstuurt. Escape schakelt binnen de editor de Neovim-modus terug; `Cmd-Escape` verlaat de editor. Moties, operators, visual-selectie, registers, zoeken en undo/redo worden door Neovim uitgevoerd. `Cmd-C/X/V` (Linux: `Ctrl-Shift-C/X/V`) gebruikt het systeemklembord; bevat het klembord een afbeelding, dan wordt die als bijlage bij de draft gezet in plaats van geplakt als tekst. Directionele paneeltoetsen nemen geen editorinvoer over.

Versturen neemt een gecontroleerde snapshot nadat Neovim de eerdere invoer verwerkt heeft. Tijdens actieve tekstcompositie of een onvoltooide Neovim-opdracht wordt niet verstuurd of naar een andere draft gewisseld. Onvoltooide IME-tekst blijft aparte recoverytekst; die wordt nooit automatisch verstuurd of in een projectbestand geschreven.

Ontbreekt Neovim of valt het proces weg, dan verschijnt een fout zonder terugval naar een tweede editor. De command palette biedt `Restart Neovim editor`, `Cancel editor command` en `Copy draft recovery`. Herstart herstelt laatst waargenomen concepttekst, niet nog onbevestigde toetsaanslagen; registers en undo-historie van het oude proces gaan verloren. Annuleren bewaart onvoltooide compositietekst apart voor herstel. Een aangepast Neovim-pad geldt bij de volgende appstart, of bij opnieuw proberen na een mislukte eerste editorstart.

### Uiterlijk

Settings groepeert de bestaande instellingen en kan zoeken op naam, beschrijving en groep. Het biedt **System**, **Light** en **Dark**, met een afzonderlijke themakeuze per modus. Save past de keuze toe; Cancel verandert het uiterlijk niet. De Neovim-prompteditor volgt dezelfde achtergrond-, tekst-, selectie- en cursorkleuren en het monospace-lettertype.

Eigen thema's staan in `<DROSTJE_DATA_DIR>/themes` (standaard `~/Library/Application Support/Drostje/themes`). Settings toont de map en kan die openen. Gebruik GPUI Kit `ThemeSet`-JSON: een object met een `themes`-array; iedere entry heeft een unieke `name` en `mode` (`light` of `dark`). Dit is niet het losse Zed-themaformaat. Ingebouwd zijn:

- **Delft Light** en **Delft Dark**, de standaard: IBM Plex Sans en Lilex, de standaardletters van Zed, met Delfts blauw als enige accentkleur.
- **Ink Light** en **Ink Dark**, een leesmodus naar e-readers: antwoorden van agents staan in Libron, code in Monaspace Xenon, met gedempte kleuren en een lichte papierkorrel op de vensterrand.
- **Cootje Light** en **Cootje Dark**, de speelse: alles in Dosis, code in Maple Mono, rondere hoeken, Cootje-blauw en saffraan voor de punt die laat zien dat een agent bezig is, en de meeste korrel.
- GPUI Kit's Default Light en Default Dark.

**Customize theme…** (command palette of het menu in de titelbalk) opent een thema-editor naar het voorbeeld van Zen Browser. Daarin kies je de weergave (System, Light, Dark), een accentkleur in een kleurenveld of uit een rij stalen, en de sterkte van de korrel. Alles is direct te zien; Save bewaart het, Cancel en Escape zetten het terug. De accentkleur geldt voor elk thema en houdt de lichtheid van dat thema aan, zodat tekst op de accentkleur even goed leesbaar blijft. Korrel ligt alleen op de vensterrand (titelbalk, Projects, startscherm), nooit achter gesprekken, de editor of code. Reset gaat terug naar de accentkleur en korrel van het thema zelf. Met het toetsenbord: Tab tussen de onderdelen, `h`/`l` voor tint, stalen of korrel, `j`/`k` voor intensiteit, met Shift in grotere stappen, `gg`/`G` naar de eerste of laatste staal of het minimum of maximum, en `Cmd-Return` om op te slaan.

Een eerder opgeslagen keuze voor DROSTJE. Light of DROSTJE. Dark betekent nu Delft. Het woordmerk op het startscherm staat in Dosis; zolang een turn loopt, ademt de punt van DROSTJE. naast de composer. De lettertypes zitten in de app zelf en vallen onder de SIL Open Font License; herkomst en licenties staan in `assets/fonts`.

Toegevoegde en gewijzigde bestanden worden opnieuw geladen. Ontbreekt het gekozen thema, dan gebruikt DROSTJE. een standaardthema met een waarschuwing; de opgeslagen naam blijft behouden zodat herstel van het bestand de keuze kan herstellen. Gebruik unieke namen om dubbele registry-entries te vermijden.

### Vereisten en controle

GPUI Kit is in zowel de app als de lokale terminaladapter vastgezet op `0.6.6`. De prompteditor vereist een geïnstalleerde Neovim met de benodigde embedded-RPC- en UI-API's; getest met **Neovim 0.12.3** op macOS. Het pad wordt via PATH ontdekt of expliciet in Settings opgegeven. DROSTJE. start Neovim zonder gebruikers-init, plugins, ShaDa, swap- of undobestanden en gebruikt alleen eigen scratchbuffers. Dit is geen OS-sandbox: expliciete Neovim-opdrachten draaien met je lokale rechten.

Deze GPUI Kit-versie bevat de upstream-fix voor dubbel afbrekende inline-codefragmenten. Er is geen lokale kopie of Cargo-patch van `gpui-base` meer nodig; de pixel- en tekstselectieregressie blijft onderdeel van de desktop-QA.

De OMP-integratie is getest met `18.2.0` (native RPC v2). Normale Pi gebruikt de standaard JSONL-RPC-interface; DROSTJE. hergebruikt daarvoor de gedeelde Pi/OMP-transcript-, tool-, model-, sessie- en extensie-UI-adapter zonder OMP's protocol-v2-negotiatie of gepagineerde geschiedenis te vereisen. Claude Code gebruikt de streaming JSON-interface en vraagt toestemming voor tools in DROSTJE. zelf. Authenticatie en modeltoegang moeten voor de gekozen Conversation-harness al beschikbaar zijn. Factory vereist daarnaast een compatibele CLI, Docker, de agent-image en de authenticatie/broker van de gekozen harness.

`Check readiness` toont echte `factory doctor --harness …`-uitvoer. Een Conversation gebruikt per project `omp` (standaard), normale `pi` of `claude-code`; een Factory-thread gebruikt `opencode` (standaard), `codex` of `claude-code`. Een ongeldige of niet-ondersteunde keuze blijft zichtbaar en moet vóór de volgende Conversation of Factory-run worden aangepast. DROSTJE. installeert of werkt vereisten alleen bij op jouw expliciete verzoek en voert geen authenticatie uit.

```sh
cargo check --all-targets
cargo test
cargo test --test neovim_runtime -- --ignored --nocapture
cargo test --features runtime-qa native_composition_is_recovery_only_until_committed -- --ignored --nocapture
```

De macOS-regressie `cargo test --features runtime-qa --test markdown_rendering` gebruikt echte fonts en offscreen Metal-pixels, zonder vensterbediening, schermopnametoestemming of provider-turns. Ze controleert zichtbare code-suffixen, afbreking, Unicode en tekstselectie in beide thema's.

De Servo-proeven gebruiken echte HTML-bestanden, een echt helperproces en op macOS GPUI's Metal-renderer. Ze versturen geen provider-turns:

```sh
cargo build --manifest-path crates/servo-browser/Cargo.toml --locked
cargo test --manifest-path crates/servo-browser/Cargo.toml
cargo test --manifest-path crates/servo-browser/Cargo.toml --test smoke -- --ignored --nocapture
cargo test --features runtime-qa --test servo_rendering --test servo_workspace
```

Dit controleert pixels in beide thema's, resize, klikken, tekstinvoer, adresfocus, geschiedenis, ontbrekende bestanden, de echte HTTP-toolroute, gescheiden artifact-/zijbalksessies, screenshots en het sluiten van browser-tabs zonder de Conversation te sluiten. `DROSTJE_QA_SCREENSHOTS=/tmp/drostje-browser-proof` bewaart de visuele bewijzen. Dit vervangt geen OS-inputcontrole; daarvoor moet macOS de gebruikte automatiseringstool Accessibility-toegang geven.

### Expliciete desktop-QA

Maak eerst de wegwerp-repository `FX` uit [het applicatieplan](DROSTJE_APPLICATION_PLAN.md). De normale tests gebruiken geen betaalde provider-turns. De onderstaande GPUI-QA gebruikt echte geïsoleerde processen, maar vervangt geen visuele of OS-inputcontrole.

```sh
export DROSTJE_RUNTIME_FIXTURE="$FX"
cargo test --features runtime-qa --lib workspace_actions_with_real_backends -- --ignored --nocapture
cargo test --features runtime-qa --lib editor_startup_failure_and_restart_preserve_draft -- --ignored --nocapture
cargo test --features runtime-qa --lib thread_workspace_keyboard_and_themes -- --ignored --nocapture
cargo test --features runtime-qa --lib keyboard_focus_moves_visibly_between_draft_and_projects -- --ignored --nocapture
cargo test --features runtime-qa --lib empty_workspace_actions_stay_compact -- --ignored --nocapture
cargo test --features runtime-qa --lib conversation_diff_sidebar_tracks_working_tree -- --ignored --nocapture
cargo test --features runtime-qa --lib orderly_quit_flushes_before_platform_deadline -- --ignored --nocapture
cargo test --features runtime-qa --lib factory_start_requires_persisted_configuration -- --ignored --nocapture
cargo test --features runtime-qa --lib typing_saves_on_focus_loss_and_quit_not_idle -- --ignored --nocapture
cargo test --features runtime-qa --lib factory_typing_stays_local_until_focus_loss_or_commit -- --ignored --nocapture
cargo test --features runtime-qa --lib spaces_preserve_drafts_tabs_and_shared_projects -- --ignored --nocapture
cargo test --features runtime-qa --lib space_swipes_track_reverse_cancel_and_settle -- --ignored --nocapture
cargo test --features runtime-qa --lib spaces_share_sidebar_geometry_and_visibility -- --ignored --nocapture
cargo test --features runtime-qa --lib thread_sidebar_hover_actions_rename_and_archive_immediately -- --ignored --nocapture
cargo test --features runtime-qa --lib thread_archive_action_guards_and_palette_are_immediate -- --ignored --nocapture
cargo test --features runtime-qa --lib terminal_tabs_keep_failure_local_and_do_not_persist_sessions -- --ignored --nocapture
```

Deze scenario's versturen geen provider-turns en starten geen remote Factory-run. Ze controleren native draftwissels, nieuwe regels, live Git-wijzigingen, bescherming tegen verouderde acknowledgements, openen/sluiten/herstellen van threadtabs, beide threadsoorten, palette-/focusgedrag, themawijzigingen, veilige afsluiting en de opslagbarrière vóór Factory Start.

Het terminalscenario gebruikt het GPUI-testvenster zonder native terminalbackend: het controleert foutisolatie, toetsenbordnavigatie tussen Factory en Terminal, en het uitsluiten van terminalsessies uit opgeslagen layouts. Renderer, shellinvoer, klembord en procesbeëindiging moeten daarnaast in een echt macOS- of Wayland-venster worden gecontroleerd. De bridge-regressies draaien met `cargo test -p ghostty-terminal`.

De macOS-login-shellregressie start een echte zsh met geïsoleerde profielbestanden en controleert de laadvolgorde: `zig test crates/ghostty-terminal/vendor/ghostty/src/config/default_shell.zig`.

Het Space-scenario controleert aanmaken, dubbele namen, hernoemen, wisselen, verplaatsen, annuleren en verwijderen, plus concepten, eigen paneelindelingen, beide thema's en het minimale vensterformaat. Gebruik daarna de afgedrukte `DROSTJE_SPACES_QA_DATA`-map voor een afzonderlijke herstartcontrole:

```sh
export DROSTJE_SPACES_QA_DATA="/pad/uit/het/space-scenario"
cargo test --features runtime-qa --lib spaces_restore_active_context_and_inactive_tabs -- --ignored --nocapture
```

Het gedeelde-zijbalkscenario controleert breedte, zichtbaarheid en toetsenbordbediening over Space-wissels. Controleer de opslag in een nieuw proces met de afgedrukte map:

```sh
export DROSTJE_SHARED_SIDEBAR_QA_DATA="/pad/uit/het/gedeelde-zijbalkscenario"
cargo test --features runtime-qa --lib spaces_share_sidebar_geometry_and_visibility -- --ignored --nocapture
unset DROSTJE_SHARED_SIDEBAR_QA_DATA
```

De vergelijkende prestatieworkload typt in de Neovim-composer, een Factory-invoer en Settings, en wisselt tussen tabs. Hij rapporteert mediaan en p95 van toetsafhandeling tot de GPUI-testdispatcher klaar is, niet de fysieke invoer-tot-schermlatentie. Er gelden geen tijdsdrempels in de gewone tests:

```sh
cargo test --features runtime-qa --lib interaction_performance_workload -- --ignored --nocapture
DROSTJE_PERF_HIDDEN_THREADS=8 cargo test --features runtime-qa --lib interaction_performance_workload -- --ignored --nocapture
```

De tweede variant opent acht extra echte, inactieve Conversations om werk door verborgen tabs zichtbaar te maken. Ook deze workload verstuurt geen prompts en gebruikt alleen de wegwerp-repository.

Gebruik de door de eerste QA-test gemelde datamap als `DROSTJE_QA_DATA` voor layout/herstart-controle:

```sh
cargo test --features runtime-qa --lib layout_and_factory_runtime_recovery -- --ignored --nocapture
```

De hersteltest heropent alleen de opgeslagen werkruimte. Als de fixture al launch-records bevat, worden die via de echte eventtracker in hun eigen Factory-thread getoond, zonder opnieuw te dispatchen. Exitcode 0 zonder terminal event blijft `Interrupted / Unknown`, niet Completed.

Een eerdere echte OpenCode-proef heeft dispatch, sandbox-initialisatie en GUI-herstart/herstel doorlopen. Die run pauzeerde met `PermissionDenied` / `SandboxNonZero`: Factory wees een ACP-verzoek van de agent naar `/proc/sys/kernel/random/uuid` af. Die proef bewijst geen voltooide run; de sandbox-policy is niet aangepast en deze UI-QA herhaalt die remote proef niet.

## Onder de motorkap

Rust + GPUI, macOS. Gesprekken lopen via OMP of Claude Code, prompttekst bewerk je met ingebedde Neovim binnen de GPUI Kit-composer, en klussen gaan via de `factory` CLI naar de Compliance Software Factory. Nodig: Rust, `nvim`, `factory` en minstens één van `omp` of `claude`.

---

*Koffie erbij? Dan gaan we.*
