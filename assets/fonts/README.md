# Bundled fonts

`src/ui/theme.rs` embeds these faces so every platform renders the bundled themes
the same. Each family is licensed under the SIL Open Font License 1.1; the
licence sits beside its files as `OFL.txt`.

| Family | Files | Source | Version |
|---|---|---|---|
| IBM Plex Sans | Regular, Italic, Medium, SemiBold, Bold, Bold Italic | [IBM/plex](https://github.com/IBM/plex) `@ibm/plex-sans@1.1.0`, `fonts/complete/ttf` | 1.1.0 |
| IBM Plex Mono | Regular, Italic, Bold | [IBM/plex](https://github.com/IBM/plex) `@ibm/plex-mono@2.5.0`, `fonts/complete/ttf` | 2.5.0 |
| Libron | Regular, Italic, Bold, Bold Italic | [nicoverbruggen/libron](https://github.com/nicoverbruggen/libron) `Libron.zip` | v0.31 |
| Monaspace Xenon | Regular, Italic, Bold | [githubnext/monaspace](https://github.com/githubnext/monaspace) `monaspace-static-v1.400.zip` | 1.400 |
| Dosis | SemiBold | [google/fonts](https://github.com/google/fonts/tree/main/ofl/dosis) `Dosis[wght].ttf` | see below |

Dosis ships only as a variable font. `Dosis-SemiBold.ttf` is its static instance
at weight 600, made with fontTools 4.60.1:

```sh
fonttools varLib.instancer 'Dosis[wght].ttf' wght=600 --update-name-table -o Dosis-SemiBold.ttf
```

Its licence reserves no font name, so the instance keeps the name Dosis.
GPUI's support for variable-font weights is not relied on; every face here is static.
