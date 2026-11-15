# Bundled fonts

`crates/fonts` embeds these faces (registered by `src/ui/theme/mod.rs`) so every platform renders the bundled themes
the same. Each family is licensed under the SIL Open Font License 1.1; the
licence sits beside its files as `OFL.txt`.
Delft uses Zed's default faces: IBM Plex Sans for the UI and Lilex for code.

| Family | Files | Source | Version |
|---|---|---|---|
| IBM Plex Sans | Regular, Italic, Medium, SemiBold, Bold, Bold Italic | [IBM/plex](https://github.com/IBM/plex) `@ibm/plex-sans@1.1.0`, `fonts/complete/ttf` | 1.1.0 |
| Lilex | Regular, Italic, Bold | [mishamyrt/Lilex](https://github.com/mishamyrt/Lilex) `Lilex.zip` release 2.630, `ttf` (SHA-256 `c946bf02…6728e0`) | 2.630 |
| Libron | Regular, Italic, Bold, Bold Italic | [nicoverbruggen/libron](https://github.com/nicoverbruggen/libron) `Libron.zip` | v0.31 |
| Monaspace Xenon | Regular, Italic, Bold | [githubnext/monaspace](https://github.com/githubnext/monaspace) `monaspace-static-v1.400.zip` | 1.400 |
| Dosis | Medium, SemiBold, Bold | [google/fonts](https://github.com/google/fonts/tree/main/ofl/dosis) `Dosis[wght].ttf` | see below |
| Maple Mono NL | Regular, Italic, Bold | [subframe7536/maple-font](https://github.com/subframe7536/maple-font) `MapleMonoNL-TTF-AutoHint.zip` (no ligatures, hinted; SHA-256 checked) | v7.9 |

Dosis ships only as a variable font. Its files here are static instances at
weights 500, 600 and 700, made with fontTools 4.60.1:

```sh
fonttools varLib.instancer 'Dosis[wght].ttf' wght=600 --update-name-table -o Dosis-SemiBold.ttf
```

There is deliberately no Regular: normal-weight text falls to Medium, which keeps
the narrow Dosis from going thin. Its licence reserves no font name, so the
instances keep the name Dosis.
GPUI's support for variable-font weights is not relied on; every face here is static.
