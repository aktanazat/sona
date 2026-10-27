# Translating Sona

The iPhone and Apple Watch apps use native `.strings` files. The Mac interface currently has English copy only. The old `src/i18n/locales` files belonged to the retired desktop interface and are not the place to translate the native apps.

## Source files

- iPhone: `mobile/Sona/en.lproj/Localizable.strings`
- Apple Watch: `mobile/SonaWatch/en.lproj/Localizable.strings`

Each app has its own strings. Translating the phone file does not translate the watch.

## Add a language

1. In each app you are translating, copy the English file into a sibling folder named with the language code, such as `fr.lproj/Localizable.strings`.
2. Translate values only. Keep each key, quoted-string syntax, and format placeholder such as `%d` or `%@` intact.
3. Build the iPhone app using `BUILD.md`. Set the simulator's language to the new language, then check the consent sheet, recording controls, notes, empty states, and error messages. Check the watch separately if you translated its file.
4. Submit the translated files in a pull request and say which screens you checked.

Use natural, concise wording. Do not translate Sona or provider names. The available native translations are the files actually present in the app targets; the retired desktop's language list does not describe them.
