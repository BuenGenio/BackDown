---
title: Люди, бази даних і пошук усередині речей
description: Побачте всіх людей і кожну базу даних, яку зберігають ваші диски, зібраних з усіх дисків — з поштових сховищ, даних Nextcloud, домашніх тек, конфігурацій сайтів і дампів; а потім шукайте всередині пошти та баз даних, використовуючи індекси, які вже є.
seo_title: Люди, бази даних і пошта на старих дисках, пошук усередині
---

Шухляда зі старими дисками зберігає тих самих людей і ті самі бази даних багато разів: скринька Аліси у сховищі поштового сервера, її облік Nextcloud у пізнішій резервній копії, її домашня тека в образі системи; база даних магазину, дамп якої роблять щоночі вже роками, її робочий каталог даних у резервній копії сервера, і сайт на WordPress, який її використовував. BackDown зводить кожного з них в одну **людину** і одну **базу даних**, попри різні диски, і дозволяє шукати всередині самої пошти та баз даних.

Обидва походять із [того, що розпізнано](/docs/concepts/#recognised-things-and-provenance), тож спершу проіндексуйте й захешуйте диски — демон зробить решту після кожного сканування.

## Люди

```sh
backdown people
backdown people alice
```

```text
Alice Example
  Email addresses: alice@example.org
  Logins: alice on cloud.example.org
  Mailboxes: 1, 12,040 messages, 4.1 GB, 2011–2020
  Nextcloud accounts: 1, 8.2 GB
  Home folders: 1, 1.3 GB
  Drives: 3
  ↳ Mailbox            12,040 messages                          BD-500:backups/mail/vmail/example.org/alice/Maildir
  ↳ Nextcloud account  8.2 GB of files on cloud.example.org    NAS:cloud/data/alice
  ↳ Home folder        Linux home on tomahawk                   OLD-LAPTOP:home/alice
```

Людину знаходять у:

- **поштовому сховищі**: `<domain>/<user>` у сховищі Dovecot чи mailcow стає `user@domain`; окремий Maildir сам називає свого користувача;
- **каталозі даних Nextcloud**: кожна тека користувача з `files/` усередині; коли на диску є ще й `config/config.php` сервера, його `mail_domain` дає адресу, а `trusted_domains` — інстанцію;
- **домашній теці** в образі системи, з іменем хоста машини;
- **профілі Thunderbird**: особи (імена й адреси) у `prefs.js`;
- **конфігурації сайту**: контакт, названий у `configuration.php` Joomla чи `.env` Laravel.

Та сама електронна адреса, або той самий логін на тому самому хості, де завгодно — це та сама людина; саме лише ім'я двох людей ніколи не об'єднує. Кожна людина показує все, що про неї є, у яких місцях, на яких дисках. У застосунку **What's here** має вкладку **People** з тим самим, і кожне місце — посилання в браузер.

Для цього нічого не читається всередині листів чи файлів: імена беруться зі шляхів і з невеликих конфігураційних файлів вище, лише з підключених дисків.

<figure class="demo" data-view="entities" data-caption="Розпізнані речі: хто і що знайдено на дисках і звідки воно."></figure>

## Бази даних

```sh
backdown databases
backdown databases shop
```

```text
shop
  Engine: MariaDB
  Dumps: 3, 2.1 GB, 2019–2021
  Live data: 1, 2 tables, 480 MB
  Used by: WordPress site shop.example.org
  ↳ Dump         MariaDB, dumped 4 Mar 2021          BD-500:backups/db/shop_2021-03-04.sql
  ↳ Dump         MariaDB, dumped 5 Mar 2021          BD-500:backups/db/shop_2021-03-05.sql
  ↳ Live data    MariaDB, 2 tables                   OLD-SERVER:var/lib/mysql/shop
  ↳ Used by      WordPress site shop.example.org     OLD-SERVER:srv/www/shop.example.org
```

Базу даних зводять із:

- **дампів**: `mysqldump` і MariaDB, phpMyAdmin, `pg_dump` та `pg_dumpall`, `.dump` у SQLite, звичайних або стиснених; три чи більше в одній теці — це одне ціле. Заголовок називає рушій, базу даних і хост, коли диск підключено; інакше йде назва файлу без дати ("shop_2021-03-04" — це `shop`);
- **тек mongodump** (`dump/<database>/<collection>.bson`);
- **робочих каталогів даних** MySQL чи MariaDB (кожна тека з файлами таблиць — це база даних), а також каталогів даних PostgreSQL і MongoDB як окремих речей;
- **файлів SQLite**, за їхніми першими байтами, коли диск підключено; власні `.db`-файли програми всередині профілю, домашньої теки чи резервної копії телефону не рахуються;
- **того, що її використовує**: конфігурація сайту на WordPress, Joomla чи Laravel, а також сервера Nextcloud, називає базу даних, її рушій і хост.

Назви об'єднують у межах родини рушіїв: дамп `shop` для MariaDB і робочий каталог даних `shop` для MySQL — це та сама база даних; `shop` для PostgreSQL — інша. **What's here** має вкладку **Databases**.

## Пошук усередині пошти та баз даних

Поле пошуку шукає файли за назвою. Його третій обсяг, **Inside**, і `backdown search`, знаходять те, що в них усередині:

```sh
backdown search invoice 2021
backdown search "garden work" --kind message
backdown search customers --kind table
```

```text
3 matches (2 ms)
  message  Invoice for the garden work      Bob <bob@example.org> → alice@example.org   14 Jul 2017
           BD-500:backups/mail/vmail/example.org/alice/Maildir/cur/1500000000.M1P1.host:2,S
  table    customers                        shop
           BD-500:backups/db/shop_2021-03-04.sql (customers)
```

Що індексується:

- **пошта**: тема, відправник, отримувачі й дата кожного листа, з Maildir, поштових сховищ, сховищ Dovecot mdbox і файлів mbox Thunderbird;
- **дампи й файли SQLite**: кожна таблиця з її стовпцями, і які стовпці сама база даних індексувала для повнотекстового пошуку (`FULLTEXT KEY`); колекції mongodump і робочі каталоги даних MySQL — за назвою таблиці;
- на **повній глибині** — текст у рядках дампів і таблиць SQLite (лише стовпці текстового типу, до 64 МіБ на файл) і тіла листів.

Збіг відкриває теку скриньки з обраним листом, або дамп чи файл.

<figure class="demo" data-view="search" data-caption="Пошук усередині речей: пошта за темою, відправником і одержувачем; дампи та файли SQLite за таблицею та стовпцем."></figure>

### Наявні індекси використовуються повторно

- **Власний індекс Dovecot.** Поштова скринька Dovecot зберігає заголовки, які кешувала для клієнтів IMAP, у `dovecot.index.cache`. Коли цей файл відповідає `dovecot.index` скриньки, BackDown читає з нього теми, відправників, отримувачів і дати й не відкриває жодного листа: скринька на мільйон листів — це кілька сотень мегабайтів, прочитаних один раз. Листи, яких кеш не охоплює, читаються з їхніх файлів, лише заголовки. Власний індекс плагіна повнотекстового пошуку Dovecot (`fts-flatcurve`, `fts-xapian`, `fts-lucene`, Squat) помічається, але прочитати його не можна.
- **Повнотекстові таблиці SQLite.** Файл SQLite з таблицями FTS3, FTS4 чи FTS5 шукається на місці, через сам SQLite: збіг показує, скільки рядків відповідає в якій таблиці.
- **Те, що дамп каже, що проіндексував.** `FULLTEXT KEY` таблиці MySQL чи MariaDB називає стовпці, які база даних індексувала для пошуку; їх наведено разом із таблицею. Власні файли індексу рушія в каталозі даних не можна прочитати поза сервером.

### Глибина і побудова індексу

```sh
backdown search                      # what's indexed, per drive and source
backdown search --build              # index now what was scanned since
backdown search --depth full --build # the text in rows and bodies too, from now on
backdown search --depth off          # stop indexing after scans
```

`headers`, значення за умовчанням, достатньо, щоб знайти лист чи таблицю, і коштує небагато: індекс не зберігає копію тексту, лише те, де є кожен збіг, тож збіг показує тему, хто й коли, а не фрагмент. `full` росте разом із даними. Демон індексує після кожного розпізнавання, лише ті диски, що сканувалися з того часу; вид Activity показує це, а порожній результат пошуку пропонує **Index contents now**.
