---
title: Перші кроки
description: Встановіть BackDown, відкрийте його, проіндексуйте диски, подивіться, що дублюється, і дізнайтеся, чи безпечно стерти диск — у застосунку або в терміналі.
---

Перший сеанс: встановіть BackDown, відкрийте його, проіндексуйте два диски, подивіться, що дублюється, і дізнайтеся, чи безпечно стерти один із них. Усе це можна зробити у вікні застосунку або в терміналі; ця сторінка показує обидва способи. У прикладах використано старий диск, змонтований у `/media/old-drive`, і архівний диск у `/media/archive`. Використовуйте власні шляхи; у Windows — буквами дисків, наприклад `E:\`.

BackDown лише читає диски, які ви індексуєте. Єдиний файл, який він записує, — це його індекс, у вашій власній теці даних ([де зберігається індекс](/docs/troubleshooting/#where-the-index-lives)).

## Встановлення

Завантажте те, що підходить вашому комп’ютеру, зі [сторінки завантажень](/download/), а також `SHA256SUMS.txt` із того самого релізу. Звіривши файл із ним, ви переконаєтеся, що він прийшов цілим і це саме той файл із цього релізу. Починаючи з v1.0.0-beta.4, кожен реліз також має `SHA256SUMS.txt.sig` — підпис `SHA256SUMS.txt`, зроблений релізним ключем BackDown: [перевірка підпису](/docs/guides/air-gapped/#check-a-signature-without-backdown) підтверджує, що контрольні суми — саме ті, які ми опублікували. Збірки BackDown поки не підписані сертифікатом Apple чи Microsoft.

```sh
# Linux
sha256sum -c --ignore-missing SHA256SUMS.txt
# macOS
shasum -a 256 -c --ignore-missing SHA256SUMS.txt
```

```powershell
# Windows: compare this with the file's line in SHA256SUMS.txt
Get-FileHash .\BackDown-*.msi
```

| System | Installers | Or just the program |
|---|---|---|
| Linux (x86-64 та ARM64) | `.deb` (Debian, Ubuntu та споріднені), або AppImage (будь-який дистрибутив) | `backdown-…-linux-x86_64.tar.gz` або `-linux-arm64.tar.gz` |
| macOS 11 або новіша (Apple silicon та Intel) | `BackDown-….dmg`, або той самий застосунок як `BackDown-…-macos.zip` | `backdown-…-macos-universal.tar.gz` |
| Windows (x86-64) | `BackDown-…-x64.msi`, або `BackDown-…-windows-x86_64-setup.zip` | `backdown-…-windows-x86_64.zip` |

Інсталятори додають BackDown до меню застосунків, Launchpad або меню «Пуск». Звичайні архіви містять програму `backdown`, файл `README.txt` і ліцензії стороннього програмного забезпечення; команда `backdown desktop install` додає програму до застосунків пізніше. У будь-якому разі це одна програма: застосунок і команда `backdown` — це те саме.

> [!NOTE]
> Це бета-версія. Це перший реліз із десктопним застосунком і перший, чиї збірки для macOS та Windows зроблено без нашої автоматизованої служби збирання (їх крос-компілювали на Linux); їх ще не запускали на справжньому Mac чи Windows ПК. Якщо щось не працює, будь ласка, [повідомте про проблему](https://github.com/BuenGenio/backdown-releases/issues) з описом того, що ви робили і що це показало.

### Linux

`.deb` встановлює програму як `/usr/bin/backdown`, пункт меню та піктограми, а також фонову службу для кожного користувача, яку ніхто не вмикає:

```sh
sudo apt install ./backdown-io_*_amd64.deb
```

Пакет називається `backdown-io`, бо в Debian та Ubuntu вже є непов’язаний засіб видалення дублікатів під назвою `backdown`. Якщо термінал колись запропонує `sudo apt install backdown`, це інша програма. Обидва встановлюють `/usr/bin/backdown`, тож наш пакет замінює його, разом із пакетом `backdown` із BackDown beta.1 і раніших версій.

Нічого не запускається самостійно: відкрийте **BackDown** у меню застосунків. Менеджер пакетів підтримує його актуальну версію (просто завантажте новий `.deb`); команда `backdown update` лише повідомить про це, а не змінить файл, встановлений із пакета.

AppImage не потребує встановлення. Зробіть його виконуваним і запустіть: без аргументів він відкриває вікно BackDown, а з аргументами це команда `backdown`.

```sh
chmod +x BackDown-*-x86_64.AppImage
./BackDown-*-x86_64.AppImage                       # the app
./BackDown-*-x86_64.AppImage index /media/old-drive  # the command
./BackDown-*-x86_64.AppImage desktop install       # add it to the applications menu
```

AppImage не може замінити сам себе: щоб оновити, завантажте новий.

Звичайний архів — це одна статична програма. Розпакуйте її та додайте до `PATH` (без sudo також підійде `~/.local/bin`, якщо він у вашому `PATH`):

```sh
mkdir backdown && tar xzf backdown-*-linux-*.tar.gz -C backdown
sudo mv backdown/backdown /usr/local/bin/
backdown --version
backdown desktop install    # optional: the applications menu
```

### macOS

Відкрийте `.dmg` і перетягніть BackDown у Applications (або розпакуйте `.zip` і перемістіть туди `BackDown.app`). BackDown ще не нотаризований Apple, тож першого разу macOS може сказати, що не може його перевірити:

1. В Applications клацніть правою кнопкою (або Control-клацніть) на BackDown і виберіть **Open**, а потім ще раз **Open**.
2. У macOS 15 і новіших, якщо кнопки Open немає: System Settings > Privacy & Security, а потім **Open Anyway** біля імені BackDown.

Після цього він відкривається як будь-який інший застосунок. Команда `backdown` міститься всередині; щоб використовувати її в терміналі, створіть на неї посилання у вашому `PATH`:

```sh
sudo ln -s /Applications/BackDown.app/Contents/MacOS/backdown /usr/local/bin/backdown
```

Якщо натомість ви використовуєте звичайний архів, один раз видаліть атрибут карантину (або дозвольте це в System Settings > Privacy & Security після першої спроби):

```sh
mkdir backdown && tar xzf backdown-*-macos-universal.tar.gz -C backdown
xattr -d com.apple.quarantine backdown/backdown
sudo mv backdown/backdown /usr/local/bin/
backdown desktop install    # optional: ~/Applications/BackDown.app
```

### Windows

`.msi` встановлює BackDown лише для вас, без прав адміністратора: у `%LOCALAPPDATA%\Programs\BackDown`, з пунктом меню «Пуск» і доступом через `PATH` для терміналів, відкритих після цього. Установчий `.zip` робить те саме без Windows Installer: розпакуйте його і двічі клацніть **Install BackDown.cmd**; він відкриє BackDown, коли завершить. Будь-який із них видаляється в Settings > Apps.

Оскільки файли ще не підписані цифровим підписом, SmartScreen може запитати підтвердження першого разу: **More info**, потім **Run anyway**.

Зі звичайним `.zip` розпакуйте його і запустіть `backdown.exe` з терміналу (PowerShell або Command Prompt) у цій теці, або додайте теку до `PATH`; команда `backdown desktop install` додає пункт меню «Пуск».

## Відкрийте BackDown

Відкрийте **BackDown** у меню застосунків, Launchpad або меню «Пуск», або в терміналі:

```sh
backdown app
```

Це запускає фонову службу BackDown, якщо вона ще не працює (ту, яку ви встановили, або власну, якщо ні), і відкриває власне вікно. Це вікно — Chrome, Chromium, Edge або Brave, залежно від того, що у вас є, запущений як застосунок із власним профілем; якщо жодного з них немає, BackDown відкривається у вкладці вашого звичного веббраузера. Повторне відкриття просто виводить наперед уже відкрите вікно, а не запускає все вдруге.

Першого разу вкладка **Activity** вітає вас дисками, підключеними до цього комп’ютера. Натисніть **Index this** на кожному диску, про який хочете дізнатися, або введіть теку в полі "Or a folder" і натисніть **Index it**. BackDown лише читає їх. Кожен диск з’являється зліва, коли його сканування завершено, а хешування відбувається самостійно. Далі:

- **Duplicates** показує, що зберігається двічі, спочатку групи, які звільняють найбільше місця: відкрийте групу, щоб побачити, де лежить кожна копія, або згрупуйте за **Folders**, щоб побачити, які теки містять копії одна одної. Панель фільтрів звужує список за типом, розміром, датою та прихованими файлами, а позначені копії переходять до **Free up**;
- **What's here** показує, що є що: поштові скриньки, резервні копії телефонів, фотобібліотеки та інше;
- кнопка **Report** диска дає повну картину.

У Linux та macOS незмонтований диск можна змонтувати з вкладки Activity, лише для читання ([монтування дисків](/docs/concepts/#mounting-drives-read-only)).

Фонова служба продовжує працювати після закриття вікна, пересканування дисків виконується, коли це потрібно, або коли диск знову підключають. Щоб вона запускалася щоразу під час входу в систему, виконайте `backdown service install` ([тримати її запущеною](#keep-it-running)).

<figure class="demo" data-dir="/media/alex/Old Laptop" data-caption="Після індексації: диски в бічній панелі з розміром і кількістю файлів, і теки вибраного диска."></figure>

## Те саме в терміналі

### Проіндексуйте два диски

```sh
backdown index /media/old-drive /media/archive
```

```text
/media/old-drive: 44 files, 182 MB (new 44, changed 0, gone 0) · scan 1
  full walk in under a second: Linux keeps no change log for drives; not an NTFS drive
/media/archive: 39 files, 222 MB (new 39, changed 0, gone 0) · scan 2
  full walk in under a second: Linux keeps no change log for drives; not an NTFS drive
```

Кожен шлях, який ви індексуєте, стає *коренем*: диском або текою верхнього рівня, яку BackDown відстежує. Індексування записує шлях, розмір і час зміни кожного файлу. Він ще не читає вміст файлів, тож це швидко. Наведені тут цифри — з невеликої демонстрації; на справжньому диску — сотні тисяч файлів. Другий рядок повідомляє, як диск був сканований, і чому: у Windows та macOS повторне сканування часто може прочитати власний журнал змін диска замість кожної теки ([швидкі повторні сканування](/docs/concepts/#fast-rescans)).

Кожен запуск — це *сканування*. Запустіть ту саму команду пізніше, і BackDown запише, що нове, що змінилося чи зникло, збереже те, що вже знає про незмінні файли, і збереже попередні версії для [повзунка часу](/docs/concepts/#scans-and-the-time-slider).

У Windows:

```powershell
backdown index E:\ F:\Archive
```

### Подивіться, що дублюється

```sh
backdown dupes
```

`dupes` спочатку хешує все, що потенційно може бути дублікатом, потім виводить групи ідентичних файлів, спочатку ті, що марнують найбільше місця. Файл, чийого розміру не має жоден інший файл, ніколи не читається; решту вибірково перевіряють, і повністю читаються лише файли, чиї вибірки збігаються ([як працює хешування](/docs/concepts/#hashes-and-proof)).

```text
219 MB reclaimable in 32 groups of duplicates (files ≥ 1M)

120 MB  3 × 60 MB  sha256 f5511040fea1…
    /media/archive/Backups/laptop-2019/VM/win10-copy.vdi
    /media/archive/Backups/laptop-2019/VM/win10.vdi
    /media/old-drive/VM/win10.vdi
5 MB  2 × 5 MB  sha256 cccfc2d24435…
    /media/archive/Media/Photos/2014/IMG_2014003.jpg
    /media/old-drive/Photos/2014/IMG_2014003.jpg
```

`--min-size 100M` виключає менші файли; `--limit 50` виводить більше груп. Жорсткі посилання рахуються один раз, тож цифра обсягу, який можна звільнити, реальна.

<figure class="demo" data-view="dupes" data-caption="Дублікати, спершу ті, що звільнять найбільше: у кожній групі всі копії та де вони лежать."></figure>

### Чи безпечно стерти старий диск?

```sh
backdown check /media/old-drive
```

```text
/media/old-drive
  44 files, 182 MB
  verified       162 MB in 37 files (identical copy elsewhere)
  unique          20 MB in 7 files (no copy anywhere else)

COPY FIRST: 20 MB in 7 files exists nowhere else.
       19 MB         5 files  Photos
      767 KB         1 files  Documents
        14 B         1 files  .
  (--list shows every file)
```

Диск безпечно стерти, коли кожен файл на ньому має десь поза диском SHA-256-ідентичну копію. Тут таких немає в 7 файлів, тож вердикт — «copy first», разом із теками, в яких вони лежать. `--list` називає їх. Щойно їх скопійовано, а архів проіндексовано знову, та сама команда повідомляє:

```text
SAFE TO WIPE: every file has a SHA-256-identical copy outside /media/old-drive.
```

`check` завершується з кодом 0, коли диск безпечно стерти, і з кодом 2, коли ні, тож скрипти можуть це використовувати. [Що означає «безпечно стерти»](/docs/concepts/#what-safe-to-wipe-means) має подробиці, а [Звільнення диска](/docs/guides/free-up-a-drive/) продовжує звідси.

<figure class="demo" data-view="wipe" data-caption="Стирання: позначте диск, і видно, що з нього є деінде, а що — лише тут."></figure>

### Перегляньте його

```sh
backdown serve
```

```text
BackDown browser for /home/you/.local/share/backdown/index.db at
  http://127.0.0.1:8787/?t=4f0c…
(only this link, only on this computer; Ctrl-C to stop)
```

`serve` відкриває той самий браузер, що й застосунок, у вашому веббраузері, без фонової служби: кожен проіндексований диск, кожну теку такою, якою вона була на момент будь-якого сканування, кожну версію й копію файлу, а також переглядачі лише для читання для фото, PDF, документів, баз даних, коду та іншого. Він слухає лише на цьому комп’ютері (якщо ви не [попросите більше](/docs/guides/browse-without-restoring/#look-from-your-other-devices)), а посилання містить приватний токен, який змінюється щоразу. Він нічого не може змінити (копіювання, звільнення місця, порятунки та повторні сканування потребують служби), і повідомляє, яку команду запустити натомість. [Перегляд без відновлення](/docs/guides/browse-without-restoring/) показує, що він може.

<figure class="demo" data-dir="/media/alex/Old Laptop/home/alex/Pictures/Paris 2019" data-mode="gallery" data-caption="Тека з фото як галерея; подвійний клік відкриває переглядач."></figure>

### Тримайте її запущеною {#keep-it-running}

Фонова служба (*демон*) пересканує кожен диск щодня і щоразу, коли його знову підключають, хешує після кожного сканування, читає [стан](/docs/guides/drive-health/) кожного диска й тримає браузер доступним.

```sh
backdown service install
backdown open
```

`service install` запускає її від вашого імені, без додаткових прав: служба користувача systemd у Linux, LaunchAgent у macOS, і завдання за розкладом, яке запускає її під час входу у Windows. Якщо ви встановили `.deb`, служба вже є: `systemctl --user enable --now backdown` її вмикає. `backdown open` відкриває браузер у вашому звичному веббраузері, а `backdown app` — у власному вікні. [Демон](/docs/concepts/#the-daemon) пояснює, що він робить.

### Створіть звіт {#produce-a-report}

Уся картина в одному документі: кожен диск із його вердиктом, що на ньому є, яка його частина існує деінде, і найбільші дублікати.

```sh
backdown report > report.md
backdown report --html -o report.html
```

Вкладка браузера "What's here" має той самий звіт — як сторінку або завантаження у форматі Markdown. Звіт читає індекс таким, яким він є, тож спочатку запустіть `backdown dupes` або `backdown hash`, щоб врахувати кожен дублікат.

## Що далі

- [Концепції](/docs/concepts/): диски, сканування, хеші й що означає «безпечно стерти».
- [Звільнення диска](/docs/guides/free-up-a-drive/): видаляти тільки те, що має перевірену копію.
- [Занесіть старі диски в один архів](/docs/guides/file-into-an-archive/): план, а потім перевірені копії.
- [Налаштування, оновлення та ліцензії](/docs/guides/settings-and-licences/): вкладка Settings, оновлення й що безкоштовно.
- [Довідник CLI](/docs/cli/): кожна команда та опція.
