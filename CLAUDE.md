# Ferrum — заметки для Claude

RDP-клиент на GTK4/libadwaita + FreeRDP 3. Основная платформа — Linux
(Omarchy/Arch, Wayland). Сборки для Windows (MSYS2 UCRT64) и macOS делает CI
(`.github/workflows/build.yml`).

Крейты:
- `crates/freerdp-sys` — C-шим над FreeRDP (`shim/ferrum_shim.c`) и FFI.
- `crates/rdp-core` — сессия, фреймбуфер, таблица клавиш (`keymap.rs`).
- `crates/ferrum-profiles` — профили, аргументы FreeRDP (`args.rs`), .rdp/Remmina.
- `crates/ferrum` — GTK-приложение.

Проверка перед коммитом: `cargo test --workspace --locked`, `cargo build --release -p ferrum`.
Код под Windows (`#[cfg(windows)]`) на Linux не компилируется, его проверяет только
CI (job «Windows (MSYS2 UCRT64)»): после пуша смотри, что он зелёный.

## Порт на Windows (октябрь 2026, коммит b0ab58b)

Делалось на Windows-ноуте: установленная сборка падала. Что найдено и
исправлено:

1. **Падение при неудачном подключении.** FreeRDP шлёт ChannelDisconnected с
   `pInterface == NULL`, если канал так и не поднялся; `clip_uninit` делал
   `cliprdr->custom = NULL`. Добавлены проверки на NULL в `on_channel_connected`,
   `on_channel_disconnected`, `clip_uninit` (`ferrum_shim.c`).
2. **Обрыв любой сессии на Windows.** `args.rs` всегда передавал
   `/sound:sys:pulse` и `/microphone:sys:pulse`; без PulseAudio FreeRDP падает в
   `ERRCONNECT_POST_CONNECT_FAILED`. Теперь константа `AUDIO_SYS`: `:sys:pulse`
   только на Linux/BSD, иначе пусто (FreeRDP выбирает системный бэкенд).
3. **OpenSSL без MD4 (NTLM).** CI теперь кладёт `lib/ossl-modules` в пакет, а
   `main.rs` (`bundle_openssl_modules`) выставляет `OPENSSL_MODULES` на него.
4. **Клавиатура на Windows была полностью сломана.** На Windows GTK отдаёт в
   `hardware_keycode` виртуальный код (VK), а не XKB-код, и `xkb_to_rdp`
   выдавал мусор (C → F1, Ctrl → «8»), из-за этого не работал и буфер обмена
   (Ctrl+C не доходил). Сейчас:
   - `keymap::vk_to_rdp` (только Windows) — VK → сканкод через `MapVirtualKeyW`
     с явным списком расширенных клавиш (MapVirtualKey теряет E0 у стрелок и т. п.);
     в `session_view.rs` правые Shift/Ctrl/Alt и KP_Enter уточняются по keyval.
   - `crates/ferrum/src/winkbd.rs` — низкоуровневый хук `WH_KEYBOARD_LL`, как в
     mstsc. Включается из `SessionView::apply_grab` (фокус + «Capture System
     Shortcuts»), шлёт все клавиши с аппаратными сканкодами и глотает их, так что
     Alt+Tab/Win уходят на сервер. Caps/Num/Scroll Lock пропускает и локально.
     Ctrl+Alt+Enter/End/Home обрабатывает сам по `SessionView::key_held`
     (проглоченные клавиши не меняют системное состояние модификаторов). Пока хук
     активен, `on_key` клавиши не отправляет (только сочетания клиента).
   - Отправка вынесена в `SessionView::send_scancode`.
5. **Num Lock выключался на сервере при каждом фокусе (касается и Linux).**
   `ferrum_input_focus_in` слал `freerdp_input_send_focus_in_event(input, 0)`
   сразу после Sync с правильными флагами; FocusIn передаёт флаги как состояние
   индикаторов. Теперь шим запоминает `sync_flags` и отправляет их в FocusIn.
6. **Пароли на Windows не сохранялись.** `keyring.rs`: бэкенд Credential Manager
   (`CredWriteW`/`CredReadW`/`CredDeleteW`, без новых зависимостей), имя записи
   `dev.ferrum.Rdp/<rdp|gateway>/<uuid профиля>`. Тест `credential_manager_roundtrip`.
   На macOS пароли по-прежнему не запоминаются.

### Что проверено вживую (Windows 11, сервер Windows с NLA)

Подключение, ошибка неверного пароля (без падения), звук, буфер обмена в обе
стороны (текст), набор текста и смена языка.

### Не проверено / с чего продолжать

- Хук клавиатуры (`winkbd.rs`): Alt+Tab и Win на сервер, Ctrl+Alt+Home
  возвращает их ноутбуку, цифровой блок после фикса Num Lock, AltGr. Проверено
  только сборкой и тестами `keymap`.
- Сохранение пароля через диалог в реальном приложении (сам бэкенд покрыт тестом).
- Фикс Num Lock на Linux: проверить, что цифровой блок на сервере работает и
  что состояние индикаторов совпадает после переключения фокуса.
- Буфер обмена с файлами и картинками на Windows не проверялся (FUSE на Windows
  нет, `cliprdr_file_context` должен корректно отказаться).
- Шум в логе `Gtk-CRITICAL gtk_im_context_ime_message_filter` (тысячи строк) —
  баг GTK на Windows: появляется после закрытия диалога пароля. Возможный обход —
  отключить IME в Ferrum на Windows (`GTK_IM_MODULE`), но это ломает CJK-ввод в
  диалогах; пользователю предлагалось, решение не принято.
- Звук на macOS теперь без явного бэкенда (`/sound`) — проверить в сборке macOS.

### Отладка на Windows

- Сборка локально: MSYS2 UCRT64 (`pacman -S mingw-w64-ucrt-x86_64-{rust,gcc,pkgconf,freerdp,gtk4,libadwaita}`),
  затем `cargo build --release -p ferrum` и шаг Package из CI.
- Подробный лог: `FERRUM_LOG=debug WLOG_LEVEL=DEBUG`, stdout/stderr в файл.
- Краш по смещению из журнала Windows (Event 1000): символы в exe есть,
  `llvm-nm -n` / `llvm-objdump -d` на `ferrum.exe` (база 0x140000000).
- Эталон для сравнения поведения протокола: `sdl-freerdp.exe` из MSYS2 с
  `WLOG_APPENDER=FILE`.
