"""Put the live-demo figures into the docs (English and Ukrainian, same place in each: the end of the named section)."""
import re, sys
from pathlib import Path
MK = Path(__file__).resolve().parents[2]
OL = '/media/alex/Old Laptop'; HOME = OL + '/home/alex'; BK = '/media/alex/Backup 2021'
# file -> [(section heading text in English, or None = before the cards / at the end, attrs, caption en, caption uk)]
PLAN = {
 'index.md': [(None, {'dir': OL},
   "Try it here: BackDown’s own interface on an invented set of drives — an old laptop, a backup drive, camera cards, an archive, a mirror and a NAS. Open folders, look at duplicates, places, the swarm. Nothing here is yours; nothing can be changed.",
   "Спробуйте просто тут: власний інтерфейс BackDown на вигаданому наборі дисків — старий ноутбук, диск із резервними копіями, картки з камери, архів, дзеркало та NAS. Відкривайте теки, дивіться дублікати, місця, swarm. Тут немає ваших даних, і нічого не можна змінити.")],
 'getting-started.md': [
   ('Open BackDown', {'dir': OL}, "After indexing: the drives in the sidebar with their size and number of files, and the folders of the one chosen.",
    "Після індексації: диски в бічній панелі з розміром і кількістю файлів, і теки вибраного диска."),
   ("See what's duplicated", {'view': 'dupes'}, "Duplicates, the largest reclaimable first: each group lists every copy and where it is.",
    "Дублікати, спершу ті, що звільнять найбільше: у кожній групі всі копії та де вони лежать."),
   ('Is the old drive safe to wipe?', {'view': 'wipe'}, "Wipe: tick a drive to see what of it exists elsewhere and what exists only there.",
    "Стирання: позначте диск, і видно, що з нього є деінде, а що — лише тут."),
   ('Browse it', {'dir': HOME + '/Pictures/Paris 2019', 'mode': 'gallery'}, "A folder of photos as a gallery; a double click opens the viewer.",
    "Тека з фото як галерея; подвійний клік відкриває переглядач.")],
 'concepts.md': [
   ('Scans and the time slider', {'dir': HOME + '/Documents', 'mode': 'list'}, "A folder scanned twice: the time bar at the bottom steps back to the earlier scan, and Compare shows what changed.",
    "Тека, просканована двічі: шкала часу внизу повертає до попереднього сканування, а «Порівняти» показує, що змінилося."),
   ('Recognised things and provenance', {'view': 'entities'}, "What BackDown recognised on the demo drives: a Time Machine set, an iPhone backup, mailboxes, a Nextcloud data folder, a WordPress site, database dumps, libraries.",
    "Що BackDown розпізнав на демо-дисках: набір Time Machine, резервну копію iPhone, поштові скриньки, теку даних Nextcloud, сайт на WordPress, дампи баз даних, бібліотеки.")],
 'troubleshooting.md': [('The app and the background service', {'view': 'activity'}, "Activity: what the background service is doing, the connected drives and their health, notifications, mirrors and the chain of custody.",
    "Активність: що робить фонова служба, під'єднані диски та їхній стан, сповіщення, дзеркала і ланцюг зберігання доказів.")],
 'guides/browse-without-restoring.md': [
   ('Open it', {'dir': HOME}, "An indexed drive in the browser, as it was last scanned.", "Проіндексований диск у браузері, яким він був під час останнього сканування."),
   ('Find your way', {'dir': HOME + '/Pictures/Paris 2019', 'mode': 'gallery'}, "The same folders as a gallery (Ctrl+3); the size map is Ctrl+4.", "Ті самі теки як галерея (Ctrl+3); карта розмірів — Ctrl+4."),
   ('Go back in time', {'dir': HOME + '/Documents', 'mode': 'list'}, "Documents at the latest scan; drag the time bar back to see the earlier one, with what was deleted since greyed out.",
    "Documents на останньому скануванні; потягніть шкалу часу назад, щоб побачити попереднє, із тим, що відтоді видалено, сірим."),
   ('Search every drive', {'view': 'search'}, "Search: this folder, everywhere (the other machines too), or inside things.", "Пошук: ця тека, всюди (й на інших комп'ютерах) або всередині речей."),
   ('Mailboxes', {'dir': BK + '/Laptop copy 2020/Mail'}, "A Maildir opened in the browser: threads, senders and dates, read from the files as they are.",
    "Maildir, відкритий у браузері: ланцюжки, відправники й дати, прочитані з файлів як є."),
   ('Other machines', {'view': 'machine'}, "A machine in the sidebar: whether it can be reached, when it was synced, its drives, and its folders to browse.",
    "Комп'ютер у бічній панелі: чи досяжний, коли синхронізований, його диски та теки для перегляду.")],
 'guides/chain-of-custody.md': [('What is recorded, and when', {'view': 'activity'}, "Activity’s Recent, with the Chain of custody button beside it: every copy, move and merge BackDown made, sealed.",
    "«Нещодавнє» в Активності з кнопкою ланцюга зберігання поруч: кожна копія, переміщення та злиття, які зробив BackDown, запечатані.")],
 'guides/drive-health.md': [('See every drive', {'view': 'activity'}, "Connected drives in Activity: a pill beside each disk — healthy, warning, failing or no health data — and a page per drive with its readings and trend.",
    "Під'єднані диски в Активності: позначка біля кожного диска — справний, попередження, відмовляє або немає даних — і сторінка кожного диска з показниками та тенденцією.")],
 'guides/file-into-an-archive.md': [('In the browser', {'view': 'plan'}, "A plan in the browser: what goes where in the archive and why, what isn’t copied because it’s already there, and the drives’ totals.",
    "План у браузері: що куди йде в архіві й чому, що не копіюється, бо вже там є, і підсумки по дисках.")],
 'guides/free-up-a-drive.md': [('In the browser', {'view': 'wipe'}, "The Wipe view: what to wipe, what exists elsewhere and what exists only here, and what happens to the latter.",
    "Вигляд «Стирання»: що стерти, що є деінде, а що лише тут, і що станеться з останнім.")],
 'guides/manifests-and-attestations.md': [('Have the service sign them', {'view': 'settings', 'tab': 'Attestations'}, "Settings › Attestations: how often the service signs a manifest of each drive, and this installation’s signing key.",
    "Налаштування › Засвідчення: як часто служба підписує маніфест кожного диска, і ключ підпису цієї інсталяції.")],
 'guides/merge-mailboxes.md': [('What it reads', {'dir': BK + '/Laptop copy 2020/Mail'}, "One of the copies to be merged, opened in the browser: the newer snapshot of the same mailbox.",
    "Одна з копій, які зливатимуться, відкрита в браузері: новіший знімок тієї самої скриньки.")],
 'guides/mirrors.md': [('Mirrors and the background service', {'view': 'activity'}, "Activity lists each mirror with its last sync and the last verification.",
    "Активність перелічує кожне дзеркало з останньою синхронізацією та останньою перевіркою.")],
 'guides/people-databases-and-search.md': [
   ('People', {'view': 'entities'}, "Recognised things: who and what was found on the drives, and where it came from.", "Розпізнані речі: хто і що знайдено на дисках і звідки воно."),
   ('Search inside mail and databases', {'view': 'search'}, "Search inside things: mail by subject, sender and recipient; dumps and SQLite files by table and column.",
    "Пошук усередині речей: пошта за темою, відправником і одержувачем; дампи та файли SQLite за таблицею та стовпцем.")],
 'guides/recover-deleted-files.md': [("1. Find the drive's file systems", {'view': 'rescue'}, "Rescue, step 1: every disk, partition and image file, and whether BackDown can read it.",
    "Порятунок, крок 1: кожен диск, розділ і файл-образ, і чи може BackDown його прочитати.")],
 'guides/rescue.md': [('In the browser', {'view': 'rescue'}, "The Rescue view: which drive or card, what happens, and the rescues so far.",
    "Вигляд «Порятунок»: який диск чи картка, що станеться, і порятунки дотепер.")],
 'guides/settings-and-licences.md': [('The Settings view', {'view': 'settings'}, "Settings: rescans, hashing and junk rules, attestations, notifications, updates, the licence, and where the index and data live.",
    "Налаштування: повторні сканування, хешування та правила для сміття, засвідчення, сповіщення, оновлення, ліцензія, і де лежать індекс та дані.")],
 'guides/several-machines.md': [('Browse a machine', {'view': 'machine'}, "A machine’s page: reached or not, when it was synced, its drives with their health, and its folders.",
    "Сторінка комп'ютера: досяжний чи ні, коли синхронізований, його диски зі станом, і його теки.")],
 'guides/the-swarm.md': [('See the swarm', {'view': 'swarm'}, "The Swarm view: every machine’s drives, how full they are, what’s indexed on them, and the pool folders.",
    "Вигляд «Swarm»: диски кожного комп'ютера, наскільки вони заповнені, що на них проіндексовано, і теки пулу.")],
 'guides/tags-and-places.md': [('In the app: Places', {'view': 'places'}, "Places: the map drawn from the data built into BackDown, the refine list, and the photos nearest or newest first.",
    "Місця: карта, намальована з даних, вбудованих у BackDown, список уточнень і фото — найближчі або найновіші спершу.")],
 'guides/air-gapped.md': [('Carry an update over', {'view': 'settings', 'tab': 'Updates'}, "Settings › Updates: the version running, the channel, and installing from a folder carried over.",
    "Налаштування › Оновлення: яка версія працює, канал, і встановлення з перенесеної теки.")],
}
HEAD = re.compile(r'^(#{2,4})\s+(.*?)(?:\s+\{#[a-z0-9-]*\})?\s*$')

def figure(attrs, caption):
    a = ''.join(f' data-{k}="{v}"' for k, v in attrs.items())
    return f'<figure class="demo"{a} data-caption="{caption.replace(chr(34), "&quot;")}"></figure>'

def insert(path, items, en_path=None):
    lines = path.read_text(encoding='utf-8').split('\n')
    heads = [i for i, l in enumerate(lines) if HEAD.match(l)]
    en_heads = heads if en_path is None else [i for i, l in enumerate(en_path.read_text(encoding='utf-8').split('\n')) if HEAD.match(l)]
    en_lines = None if en_path is None else en_path.read_text(encoding='utf-8').split('\n')
    if '<figure class="demo"' in '\n'.join(lines): lines = [l for l in lines if not l.startswith('<figure class="demo"')]
    heads = [i for i, l in enumerate(lines) if HEAD.match(l)]
    inserts = []
    for sect, attrs, cap_en, cap_uk in items:
        cap = cap_en if en_path is None else cap_uk
        if sect is None:
            at = next((i for i, l in enumerate(lines) if l.strip() == '<!-- cards -->'), len(lines))
        else:
            src_heads, src_lines = (heads, lines) if en_path is None else (en_heads, en_lines)
            k = next(i for i, h in enumerate(src_heads) if HEAD.match(src_lines[h]).group(2) == sect)
            start = heads[k]; end = heads[k + 1] if k + 1 < len(heads) else len(lines)
            at = end
            while at > start + 1 and lines[at - 1].strip() == '': at -= 1
        inserts.append((at, figure(attrs, cap)))
    for at, fig in sorted(inserts, reverse=True):
        lines[at:at] = ['', fig, '']
    text = re.sub(r'\n{3,}', '\n\n', '\n'.join(lines))
    path.write_text(text.rstrip('\n') + '\n', encoding='utf-8')
    return len(inserts)

n = 0
for f, items in PLAN.items():
    n += insert(MK / 'docs' / f, items)
    if (MK / 'docs-uk' / f).exists():  # an untranslated guide has no Ukrainian page
        n += insert(MK / 'docs-uk' / f, items, en_path=MK / 'docs' / f)
print('figures placed:', n)
