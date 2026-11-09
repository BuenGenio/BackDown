---
title: Об'єднання поштових скриньок
description: Перетворіть копії однієї поштової скриньки, зняті в різні моменти часу, на один Maildir, де кожне повідомлення є рівно один раз і в правильній теці, і експортуйте теку як mbox. Копії лише читаються.
seo_title: Об'єднати копії поштової скриньки в один Maildir
---

Старі диски часто містять одну й ту саму поштову скриньку по кілька разів: пошту з ноутбука за 2016 рік, резервну копію із сервера за 2019-й, локальні теки Thunderbird за минулий рік. У кожній — більшість тих самих повідомлень, і в кожній — кілька таких, яких немає в інших. BackDown об'єднує їх в один Maildir, де кожне повідомлення є рівно один раз, за Message-ID, у тій теці, в якій воно є в найновішій копії, із позначками прочитаного, відповіді й прапорця з усіх копій одразу. Копії лише читаються.

## Що він читає

- **Maildir**, зокрема підтеки Maildir++ (`.Sent`, `.Archives.2019`) і розкладку "тека на теку" від Dovecot;
- **файли mbox**, один на кожну теку;
- **теку облікового запису Thunderbird** (`Mail/Local Folders` або `ImapMail/<server>` у профілі): її файли mbox і їхні підтеки `.sbd`;
- **цілі поштові сховища** — теки з Maildir, як, наприклад, `domain/user/Maildir` поштового сервера, що об'єднуються скриньку за скринькою.

Передавайте або поштові скриньки, або сховища, не змішуючи їх. Формати mdbox і sdbox від Dovecot ще не читаються.

<figure class="demo" data-dir="/media/alex/Backup 2021/Laptop copy 2020/Mail" data-caption="Одна з копій, які зливатимуться, відкрита в браузері: новіший знімок тієї самої скриньки."></figure>

## Подивіться, яким буде об'єднання

```sh
backdown mail merge /media/old-drive/Mail/ann /media/laptop-2012/Thunderbird/Mail/Local\ Folders /media/archive/Backups/mail-2021/ann/Maildir \
  --to /media/archive/Archive/Mail/ann --dry-run
```

```text
Snapshots, oldest first:
  1. /media/old-drive/Mail/ann (Maildir): 6.2 K messages, the newest from 1 Jun 2019
     only in this one: 212 messages
     folders only here: Old
  2. /media/laptop-2012/Thunderbird/Mail/Local Folders (mbox folder): 5.9 K messages, the newest from 3 Mar 2020
  3. /media/archive/Backups/mail-2021/ann/Maildir (Maildir): 6.8 K messages, the newest from 9 Feb 2021
     folders only here: Archive
Would merge 7.3 K distinct messages (1.4 GB), in 4 folders; 11.6 K duplicate copies left out
  without a Message-ID (matched by headers and body): 38
  in another folder in an older snapshot (placed where the newest has them): 144
  copies differing only in headers a server or mail program added (the newest kept): 902
  flags combined from several copies (read, replied or flagged in any): 1.1 K
  Folders: Archive 1.2 K · INBOX 5.6 K · Old 212 · Sent 310

Nothing was written. Without --dry-run, the merged mailbox goes to /media/archive/Archive/Mail/ann
```

Пробний запуск читає кожне повідомлення і нічого не записує. Копії впорядковуються за найновішим повідомленням у них, у якому б порядку ви не назвали їх.

## Як повідомлення зіставляються і розподіляються

- Дві копії вважаються одним і тим самим повідомленням, якщо в них однаковий Message-ID і однакове тіло. Той самий Message-ID з іншим тілом зберігається двічі. Повідомлення без Message-ID зіставляється за заголовками й тілом, без урахування заголовків, які додають чи змінюють сервери й поштові програми (стан, UID, ключові слова й подібне).
- Кожне повідомлення потрапляє в теку, в якій його тримає найновіша копія, що його містить, тож повідомлення, які ви розклали пізніше, опиняються розкладеними. Те, що з того часу видалили, залишається в тій теці, де було. Якщо найновіша копія тримає його в двох теках, воно опиняється в обох.
- Записане повідомлення — це повідомлення з найновішої копії, точно таким, яким воно зберігалося (з mbox — без його рядка `From ` і без цитування).
- Позначки прочитаного, відповіді, прапорця й переслання об'єднуються з усіх копій; чернетка та кошик беруться з тієї копії, яку залишають.

У поштовому сховищі повідомлення зіставляються скриньку за скринькою: те саме повідомлення в поштових скриньках двох людей залишається в обох.

## Об'єднайте

```sh
backdown mail merge /media/old-drive/Mail/ann /media/laptop-2012/Thunderbird/Mail/Local\ Folders /media/archive/Backups/mail-2021/ann/Maildir \
  --to /media/archive/Archive/Mail/ann
```

Місце призначення має бути новою або порожньою текою (або такою, яку вже почало це об'єднання). Вона стає скринькою Maildir++: вхідні — це сама тека, а інші теки — `.Name` всередині неї (`.Archives.2019` для вкладених), розкладка, якою користуються Dovecot і Courier і яку розуміють більшість поштових серверів і програм, що читають Maildir. Кожне повідомлення записується під тимчасовим іменем, зберігається на диск, переміщується на місце, а потім читається назад і порівнюється, і журналюється в індексі BackDown. Перерване об'єднання продовжується з того місця, де зупинилося, коли ви запускаєте ту саму команду знову. Воно завершується повідомленням "Written and verified" і виходить з кодом 2, якщо якесь повідомлення не вдалося прочитати чи записати.

Коли фонова служба запущена, об'єднання передається їй (воно показується в Activity як "Merge mailboxes"), а диск місця призначення індексується після цього.

## У плані розкладання

[План розкладання](/docs/guides/file-into-an-archive/) помічає поштові скриньки одного походження, які щонайменше на 90% однакові (той самий Maildir чи поштове сховище на двох старих дисках, наприклад). За замовчуванням він розкладає одну і копіює лише додаткові файли з іншої поруч із нею, в `Also from <drive>/…`. Щоб замість цього об'єднати їх:

- `backdown plan --to /media/archive/Archive --merge-mail` об'єднує кожну таку пару;
- у вигляді Plan кнопка **Merge these mailboxes** на елементі робить це для нього одного, ще до застосування плану.

Застосування плану тоді записує одну об'єднану поштову скриньку для кожної такої пари ("Merge snapshots of a mailbox (by Message-ID)" у плані), сховище за сховищем і скринька за скринькою. Пробний запуск плану не читає повідомлення; `backdown mail merge … --dry-run` на тих самих теках показує, яким буде об'єднання.

## Експортуйте теку як mbox

```sh
backdown mail export /media/archive/Archive/Mail/ann/.Sent --mbox /media/archive/Exports/ann-sent.mbox
```

```text
exported 310 messages (48 MB) to /media/archive/Exports/ann-sent.mbox as mboxrd, read back and verified
Only this folder's messages (not its subfolders); read, replied and flagged marks aren't kept: mbox has no standard place for them.
```

Це записує власні повідомлення однієї теки Maildir, від найстаріших, як файл mbox (різновид mboxrd, який цитує рядки `From `, щоб вони повернулися точно такими ж). Файл не повинен існувати раніше і не може бути всередині Maildir. Він записується під тимчасовим іменем і читається назад, перш ніж отримати своє ім'я.

`backdown mail` безплатний; об'єднання в плані — частина плану розкладання, який позначено як Pro ([Free та Pro](/docs/guides/settings-and-licences/#free-and-pro)).
