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

## Onder de motorkap

Rust + GPUI, macOS. Gesprekken lopen via OMP, tekst typ je in een ingebedde Neovim, en klussen gaan via de `factory` CLI naar de Compliance Software Factory. Nodig: Rust, `nvim`, `omp`, `factory`.

---

*Koffie erbij? Dan gaan we.*
