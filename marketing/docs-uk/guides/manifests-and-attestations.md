---
title: Підписані маніфести та засвідчення
nav: Маніфести та засвідчення
description: Підпишіть твердження про те, що саме диск містив на момент сканування, яке будь-хто може перевірити на незмінність, нехай служба підписує таке після кожного сканування чи за розкладом, і перевіряйте його на іншому комп'ютері — з BackDown або без нього.
---

**Маніфест** — підписане твердження про те, що саме один диск містив на момент одного сканування: шлях, розмір і хеш кожного файлу, запечатані в єдиний відбиток (корінь Merkle) і підписані власним ключем вашого BackDown. Зміните в ньому щось потім — ім'я, розмір, хеш, дату чи кількість — і він більше не проходить перевірку. Маніфести кожного диска зв'язані в ланцюг, кожен називає попередній, тож пропущений чи переставлені два теж буде видно.

Вони потрібні, коли комусь іншому треба покладатися на те, що містив диск: передача диска, архівування перед стиранням, зберігання доказів або подальше доведення, що в архіві нічого не змінилося. **Засвідчення** — це маніфест, який фонова служба підписує сама, після кожного сканування чи за розкладом.

Самостійне підписання маніфестів позначено як Forensic, а засвідчення за розкладом — як Team; у цій бета-версії все розблоковано ([Free та Pro](/docs/guides/settings-and-licences/#free-and-pro)). Перевірка маніфеста безплатна для будь-кого і не потребує індексу: `backdown manifest verify`, або кілька рядків Python ([нижче](#without-backdown)).

## Що маніфест доводить, і що ні

Маніфест, що проходить перевірку, доводить, що **власник ключа підпису цього встановлення бачив саме ці файли, із саме цими розмірами й саме цими хешами, у той момент**: що на скануванні 17 диска `/media/old-drive` індекс BackDown містив точно ті файли, які називає його перелік, не більше й не менше, і що в документі нічого не змінилося від моменту підписання. Перелік, який дає підписаний корінь Merkle, перелічує точно ці файли; зміните хоч один байт у ньому — і він дасть інший корінь.

Його межі:

- **Файли, які не хешувалися, засвідчені лише за шляхом і розміром.** BackDown [читає лише те, що може бути дублікатом](/docs/concepts/#hashes-and-proof), тож файл, чий розмір не поділяє жоден інший файл, ніколи не читається, і його рядок у переліку каже `"sha256": null`. Зміна такого файлу, що зберегла його розмір, не буде помітна. Маніфест рахує такі файли (`unhashed`), і `backdown manifest create` повідомляє, скільки їх. Хеш порожнього файлу відомий без його читання, тож він рахується як хешований.
- **Це те, що містив індекс.** Сканування записує кожен файл, який може прочитати в теці диска; той, який не вдалося прочитати, або той, що на іншій файловій системі, змонтованій усередині (якщо не індексувати з `--cross-filesystems`), у маніфест не входить.
- **Це не доводить, що власник ключа чесний.** Це доводить, який ключ підписав. Хто контролює комп'ютер, його індекс і його ключ, той вирішує, що підписувати, а дати в документі — з годинника цього комп'ютера. Щоб прив'язати ключ до особи, вона дає вам його відбиток наперед, якимось іншим способом (`backdown manifest key`). Щоб довести, що маніфест існував до певного дня, покладіть його корінь Merkle або SHA-256 файлу десь, де зберігаються дати, яким ви довіряєте: у листі, тікеті чи службі штампування часу.

## Підпишіть один

```sh
backdown manifest create /media/old-drive --out old-drive.manifest.json --list old-drive.listing.jsonl
```

```text
Signed manifest 4 of /media/old-drive at scan 17: 182.4 K files, 412.6 GB; 151.0 K hashed, 31.4 K by path and size only
  Merkle root 6a1e0c9b3d4f…
  follows manifest 3 of this drive · key 37f2-ec0e-8ce7-d314
  document: old-drive.manifest.json
  listing:  old-drive.listing.jsonl
```

Це засвідчує останнє завершене сканування диска, зчитане з індексу, тож диск не обов'язково має бути підключений; `--scan N` засвідчує більш раннє (`backdown scans` перелічує їх). Тека на диску теж називає диск. Прохешуйте спершу, якщо хочете засвідчити якомога більше за вмістом: `backdown hash`, або дайте фоновій службі закінчити це після свого сканування.

**Документ** — невеликий JSON; без `--out` він виводиться в термінал. **Перелік** має рядок для кожного файлу — саме це дозволяє перевірити, *які* файли підписано; зберігайте його разом із документом. Жоден із цих файлів ніколи не перезаписується. Кожен маніфест також зберігається в індексі, зв'язаний із попереднім маніфестом цього диска, а перший, який ви підписуєте, створює ваш ключ ([нижче](#the-signing-key)).

## Нехай служба підписує їх

У **Settings > Attestations**, "Sign a manifest of each drive":

- **Off** — за замовчуванням;
- **After each scan** — щойно завершаться сканування диска й хешування після нього;
- **Once a day**, **Once a week** — коли найновішому маніфесту диска стільки часу.

Маніфест робиться лише зі сканування, яке ще не покрив жоден маніфест: диск, який не пересканували, повторно не підписується. Activity перелічує кожне таке як "Sign a manifest of *drive*", із тим, що підписано. У верхній частині диска в браузері панель відомостей (<kbd>Ctrl+I</kbd>) перелічує його підписані маніфести, від найновішого, кожен із кнопкою **Download** для підписаного документа.

Загальносистемна служба ([Settings](/docs/guides/settings-and-licences/#a-machine-wide-service)) підписує власним ключем, що зберігається в `/var/lib/backdown` і доступний для читання лише її користувачу `backdown`, а не вашому; Settings показує, який це ключ. Якщо вона не може зберегти там ключ, вона не підписує, а її журнал повідомляє, чому.

## Перелічіть їх і дістаньте один

```sh
backdown manifest list
```

```text
  ID  SIGNED                  SCAN      FILES       SIZE  HASHED  KEY                  DRIVE
   3  2026-09-24 09:20 UTC      12   181.9 K   411.8 GB     82%  37f2-ec0e-8ce7-d314  /media/old-drive
   4  2026-10-01 09:20 UTC      17   182.4 K   412.6 GB     82%  37f2-ec0e-8ce7-d314  /media/old-drive

/media/old-drive: 2 manifests, the chain is whole
```

`backdown manifest list /media/old-drive` показує маніфести одного диска. Виходить з кодом 2, коли ланцюг розірвано.

У маніфеста, який підписала служба, ще немає переліку. `export` виводить будь-який збережений маніфест і створює його перелік знову з запису сканування в індексі, з хешами, які були на момент підписання (індекс пам'ятає, у яких файлів хешу не було):

```sh
backdown manifest export 4 --out old-drive.manifest.json --list old-drive.listing.jsonl
```

Команда відмовляється, якщо запис індексу вже не дає підписаного кореня — це означає, що індекс з того часу змінився.

## Перевірте один

```sh
backdown manifest verify old-drive.manifest.json --list old-drive.listing.jsonl
```

```text
old-drive.manifest.json:
  Signature: good, by key 37f2-ec0e-8ce7-d314 (this installation's own)
  A manifest of /media/old-drive (drive 3) at scan 17, which finished 2026-10-01T09:12:44Z; signed 2026-10-01T09:20:03Z
  182.4 K files, 412.6 GB: 151.0 K hashed, 31.4 K attested by path and size only
  Chain: kept here as manifest 4, following manifest 3
  Listing: good: its 182.4 K files give the signed Merkle root
```

Це перевіряє підпис, а з `--list` — що перелік точно те, що було підписано: ті самі файли, розміри й хеші, у тому самому порядку. Де індекс тут зберігає маніфести диска, перевіряється ланцюг проти них. Маючи кілька маніфестів диска, у будь-якому порядку, команда ставить їх у порядок ланцюга й перевіряє, що кожен слідує за попереднім:

```sh
backdown manifest verify old-drive-*.manifest.json
```

Пропущений показується як «розрив», а два переставлені — як «не в порядку». `verify` виходить з кодом 0, коли все проходить перевірку, і з кодом 2, коли щось не проходить; `--json` дає все це у форматі JSON.

`--against-index` ще й порівнює маніфест із тим, що індекс зараз містить про диск: що додано, вилучено і змінено з того часу. Без `--list` сторона маніфеста береться із запису сканування в індексі, і спершу повідомляється, чи цей запис усе ще дає підписаний корінь, що показує, чи змінювалася історія індексу:

```text
  Compared with the index: /media/old-drive at scan 21 (2026-10-03 08:00 UTC)
    The index's record of scan 17 still gives the signed Merkle root: it holds exactly what was signed.
    since then: 12 added (48 MB), 1 removed (2 MB), 1 changed; 182.4 K unchanged, 31.4 K of them the same size but not hashed on both sides
    added    Photos/2026/IMG_2210.jpg
    removed  Notes/old.txt
    changed  Documents/report.odt  (24 KB 942854ac803d… → 25 KB 6dbfea99b2f0…)
```

Файл рахується як змінений, коли розмір відрізняється, або коли обидві сторони мають хеш і вони відрізняються.

### На іншому комп'ютері

Скопіюйте туди документ і його перелік і запустіть там `backdown manifest verify`: це не потребує індексу, і перевірка безплатна в будь-якій редакції. Потім порівняйте відбиток ключа з тим, який вам дав той, хто підписував. "this installation's own" з'являється лише на комп'ютері, який його підписав.

### Без BackDown

Формат відкритий ([нижче](#the-format)), тож перевірити маніфест може будь-що, що вміє SHA-256 і Ed25519. Цей Python перевіряє підпис (пакетом `cryptography`) і перелік:

```python
#!/usr/bin/env python3
"""Check a BackDown manifest without BackDown: python3 verify_manifest.py MANIFEST.json LISTING.jsonl"""
import base64, hashlib, json, struct, sys

H = lambda b: hashlib.sha256(b).digest()
b64 = lambda s: base64.urlsafe_b64decode(s + "=" * (-len(s) % 4))
doc = json.load(open(sys.argv[1], encoding="utf-8"))
ok = True

# 1. the signature: Ed25519, over the canonical form (keys sorted, no spaces) without "sig"
try:
    from cryptography.exceptions import InvalidSignature
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey
    unsigned = {k: v for k, v in doc.items() if k != "sig"}
    canonical = json.dumps(unsigned, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
    try:
        key = Ed25519PublicKey.from_public_bytes(bytes.fromhex(doc["key"]))
        key.verify(b64(doc["sig"]), canonical.encode("utf-8"))
        print("signature: good, by key", doc["key"])
    except InvalidSignature:
        print("signature: DOESN'T MATCH: the document changed after it was signed")
        ok = False
except ImportError:
    print("signature: not checked (pip install cryptography)")

# 2. the listing: one leaf per line, in path-byte order, giving the signed Merkle root
level, last, files, size = [], None, 0, 0
for line in open(sys.argv[2], encoding="utf-8"):
    if not line.strip():
        continue
    f = json.loads(line)
    path = b64(f["path_b64"]) if "path_b64" in f else f["path"].encode("utf-8")
    if last is not None and path <= last:
        sys.exit(f"listing: out of order at {f['path']}")
    last = path
    digest = bytes.fromhex(f["sha256"]) if f["sha256"] else bytes(32)
    level.append(H(b"\x00" + struct.pack(">I", len(path)) + path + struct.pack(">Q", f["size"]) + digest))
    files, size = files + 1, size + f["size"]
while len(level) > 1:
    up = [H(b"\x01" + level[i] + level[i + 1]) for i in range(0, len(level) - 1, 2)]
    if len(level) % 2:
        up.append(level[-1])  # an odd node at the end of a level is carried up unchanged
    level = up
root = level[0] if level else H(b"")
same = root.hex() == doc["merkle"] and files == doc["counts"]["files"] and size == doc["counts"]["bytes"]
print(f"listing: {files} files, {size} bytes, Merkle root {root.hex()}:", "matches" if same else "DOESN'T MATCH")
sys.exit(0 if ok and same else 2)
```

## Ключ підпису

Кожне встановлення BackDown створює власний ключ Ed25519 першого разу, коли підписує, і зберігає його у своїй теці даних як `signing.key`, доступний для читання лише вам. Він ніколи не залишає комп'ютер.

```sh
backdown manifest key
```

```text
This installation's signing key, kept in ~/.local/share/backdown/signing.key (readable by you alone):
  fingerprint  37f2-ec0e-8ce7-d314
  public key   918dfc0ea3a6c491479b5ca11e3df7f2a10ff812adf042449e74fc7fe0eb6b55
```

Кожен маніфест називає відкритий ключ, яким підписаний. Відбиток — це перші 16 шістнадцяткових цифр SHA-256 від шістнадцяткового запису відкритого ключа, групами по чотири: достатньо короткий, щоб продиктувати по телефону. Зробіть резервну копію ключа разом з рештою теки даних: новий ключ починає нові ланцюги, бо від імені старого вже нічого не підписати.

## Формат

### Перелік

[JSON Lines](https://jsonlines.org/): один файл на рядок, у порядку листків дерева.

```text
{"path":"Documents/report.odt","size":24576,"sha256":"942854ac803d5a3c54ac30beaaddd5b344181aa5bccb071c4e3652c2ed75359e"}
{"path":"Notes/caf�.txt","path_b64":"Tm90ZXMvY2Fm6S50eHQ","size":9,"sha256":null}
{"path":"empty.txt","size":0,"sha256":"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"}
```

- `path` — відносний шлях на диску, з роздільником системи, яка його створила (`/`, або `\` у Windows), точно так, як його записує диск (без нормалізації).
- Ім'я, що не є коректним Unicode, як, наприклад, ім'я Latin-1 зі старого диска, не можна записати точно як рядок JSON. У такому рядку є ще й `path_b64` — байти шляху в base64url без доповнення, а його `path` тоді лише читабельна заміна.
- `size` у байтах. `sha256` — SHA-256 вмісту в шістнадцятковому вигляді, або `null`, коли не хешувався.

### Корінь Merkle (`backdown-merkle-v1`)

1. **Байти шляху** файлу — це байти UTF-8 поля `path`, або байти, які дає декодування `path_b64`, коли воно є. (Для імені, що не є коректним Unicode, це байти, які зберігає система: власні байти імені в Linux і macOS, його UTF-16LE у Windows.)
2. Листки впорядковані за байтами шляху, порівняними як неозначені байти. Кожен шлях з'являється один раз.
3. **Листок** — це `SHA-256(0x00 ‖ length ‖ path bytes ‖ size ‖ hash)`: `length` — кількість байтів шляху як 4 байти у форматі big-endian, `size` — розмір файлу як 8 байтів big-endian, `hash` — 32 байти його SHA-256, або 32 нульові байти, коли файл не хешувався.
4. **Внутрішній вузол** — це `SHA-256(0x01 ‖ left ‖ right)`. Кожен рівень парує свої вузли зліва; непарний вузол у кінці рівня переноситься на наступний рівень без змін.
5. **Корінь** — єдиний вузол, що залишився нагорі. Диск без файлів має коренем SHA-256 нічого (`e3b0c442…b855`).

Наприклад, три файли `a.txt` (3 байти, SHA-256 рядка `abc`), `b/c` (порожній) і `d` (5 байтів, не хешований) дають корінь `6dc17965ade625c342c4ac85bf48b5a5a844e335ad00f3f452dd6a7b8ba8d675`: листки `a.txt` і `b/c` парують, а листок `d`, що лишився, доєднується до них нагорі.

### Документ

```json
{
  "algorithm": "backdown-merkle-v1",
  "counts": { "bytes": 412612345678, "files": 182431, "hashed": 151012, "unhashed": 31419 },
  "created": "2026-10-01T09:20:03Z",
  "drive": { "path": "/media/old-drive", "root_id": 3 },
  "key": "918dfc0ea3a6c491479b5ca11e3df7f2a10ff812adf042449e74fc7fe0eb6b55",
  "kind": "backdown-manifest",
  "merkle": "6a1e0c9b3d4f…",
  "prev": "c0ffee52e9a7…",
  "scan": { "finished_at": "2026-10-01T09:12:44Z", "id": 17 },
  "sig": "8oiPdgZ1YopwqG7mjw-1-5qaUMNqmFPKime-…",
  "v": 1
}
```

- `drive` — тека диска на комп'ютері, що підписав (із `path_b64` теж, коли це не коректний Unicode), і його номер у цьому індексі. `scan` — засвідчене сканування і коли воно завершилося; `created` — коли маніфест підписано, за UTC.
- `counts`: файли, їхній загальний розмір і скільки було й не було хешовано.
- `key` — відкритий ключ Ed25519, яким підписано, у шістнадцятковому вигляді. `sig` — підпис Ed25519, у base64url без доповнення, **канонічної форми** документа без `sig`: його JSON з ключами кожного об'єкта, впорядкованими (за їхніми байтами UTF-8), без пробілів чи переходів рядка, рядками й числами, записаними так, як це робить звичайний JSON, і символами поза ASCII такими, як вони є.
- `prev` — SHA-256, у шістнадцятковому вигляді, канонічної форми попереднього маніфеста диска, цілого (разом із його `sig`), або `null` для першого маніфеста диска. Це і є ланцюг: маніфест не можна вилучити з нього, поставити в інше місце чи змінити без того, щоб `prev` наступного більше не збігався.
