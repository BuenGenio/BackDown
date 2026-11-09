---
title: Фізично ізольовані комп'ютери
nav: Фізично ізольовані комп'ютери
description: BackDown на комп'ютері без мережі — контрольний список, ліцензії, що перевіряються офлайн, і оновлення, принесені на диску, з перевіркою підпису перед встановленням.
---

BackDown робить свою роботу без мережі: індексація, хешування, дублікати, перевірки, перегляд, звільнення дисків, rescue і стан дисків — усе це відбувається на тому комп'ютері, на якому він працює. Це робить його придатним для комп'ютера, що тримається поза будь-якою мережею, як-от робоча станція для доказів чи лабораторний комп'ютер. Ліцензія перевіряється на самому комп'ютері, а оновлення можуть приходити на диску, який ви принесете, звірені з підписом BackDown перед тим, як щось встановлюється.

Встановлення оновлень із теки (`backdown update --from`) — частина **BackDown Forensic** ("Air-gapped deployment"); у цій беті все розблоковано ([Безкоштовне і Pro](/docs/guides/settings-and-licences/#free-and-pro)). Перевірка принесеної теки (`--from … --check`) і решта цієї сторінки працюють у кожному виданні.

## Контрольний список

На фізично ізольованому комп'ютері:

1. **Перевірки оновлень вимкнені.** BackDown не шукає оновлення, доки хтось це не увімкнув. Щоб переконатися, перевірте, що **Look for updates once a day** не позначено в Settings > Updates. У терміналі це налаштування — `update_check` у `settings.json` у вашій [теці даних](/docs/guides/settings-and-licences/#the-index): відсутнє або `false` означає вимкнено. Якщо поряд немає `update.json`, BackDown ніколи не питав. На Linux:

   ```sh
   grep update_check ~/.local/share/backdown/settings.json   # nothing, or "update_check": false
   ls ~/.local/share/backdown/update.json                    # No such file: it never asked
   ```

   Щоб унеможливити це, хоч би що казали Settings, запустіть демон із `BACKDOWN_NO_UPDATE_CHECK=1` у його середовищі (на Linux — `systemctl --user edit backdown`, потім `Environment=BACKDOWN_NO_UPDATE_CHECK=1` під `[Service]`). Не натискайте **Check now**: без мережі це просто не вдасться, але це все одно запит.
2. **Жодного іншого комп'ютера не додано.** `backdown remote list` нічого не перелічує, і розділ бічної панелі **Machines** порожній. Щойно комп'ютер додано, демон синхронізує його через ssh кожні кілька годин, тож не додавайте тут нікого. `backdown remote remove NAME` забуває комп'ютер ([Кілька комп'ютерів](/docs/guides/several-machines/)).
3. **Ніщо не слухає за межами цього комп'ютера.** Браузер і демон слухають лише на 127.0.0.1, якщо їх не запущено з `--host`, тож не передавайте тут `--host`.
4. **Ліцензія перевіряється офлайн.** Ліцензійний ключ перевіряється проти підпису на самому комп'ютері, без обліку й без мережі. Принесіть його в текстовому файлі й додайте звідти:

   ```sh
   backdown license add - < /media/usb/licence.txt
   backdown license show
   ```

5. **Оновлення приходять з теки, яку ви приносите**, через `backdown update --from` ([нижче](#carry-an-update-over)).
6. **Перевірте принесену теку, перш ніж встановлювати з неї**, через `backdown update --from FOLDER --check`: на підключеному до мережі комп'ютері, перш ніж принести її, і ще раз на фізично ізольованому.

На Linux загальносистемну службу відрізає від будь-якої мережі за межами комп'ютера сам systemd, хоч би що робив BackDown ([загальносистемна служба](/docs/guides/settings-and-licences/#a-machine-wide-service)).

## Принести оновлення {#carry-an-update-over}

На комп'ютері з мережею відкрийте реліз на [сторінці релізів BackDown](https://github.com/BuenGenio/backdown-releases/releases) і завантажте три файли в одну теку, наприклад на USB-диск:

- програму для системи фізично ізольованого комп'ютера: `backdown-v…-linux-x86_64.tar.gz`, `-linux-arm64.tar.gz`, `-macos-universal.tar.gz` чи `-windows-x86_64.zip`;
- `SHA256SUMS.txt`, SHA-256 кожного файлу релізу;
- `SHA256SUMS.txt.sig`, підпис `SHA256SUMS.txt`, зроблений ключем релізу BackDown.

Якщо BackDown там було встановлено з пакунка (`.deb`, AppImage, `.dmg` чи `.msi`), візьміть новий пакунок замість програми, або разом з нею.

Перевірте теку, перш ніж принести її:

```sh
backdown update --from /media/usb --check
```

```text
  SHA256SUMS.txt is signed with BackDown's release key
  ok  backdown-v1.0.0-linux-x86_64.tar.gz
1 file in /media/usb matches SHA256SUMS.txt, which is signed with BackDown's release key.
Here (linux-arm64): there's no backdown-v1.0.0-linux-arm64.tar.gz in the folder, so it can't update this computer (it can still update others).
Nothing was changed.
```

`--check` перевіряє кожен файл у теці, який перелічує `SHA256SUMS.txt`, для будь-якої системи, і каже, яких файлів він не перелічує. Це нічого не змінює і завершується з кодом 1, якщо щось не збігається.

Потім, на фізично ізольованому комп'ютері:

```sh
backdown update --from /media/usb --check
backdown update --from /media/usb
```

```text
  SHA256SUMS.txt is signed with BackDown's release key
  SHA-256 of backdown-v1.0.0-linux-x86_64.tar.gz matches SHA256SUMS.txt
  installed BackDown 1.0.0 in /home/you/.local/bin/backdown
Updated BackDown 1.0.0-beta.4 → 1.0.0 (from /media/usb).
restarted the BackDown service
```

У такому порядку, він:

1. перевіряє підпис `SHA256SUMS.txt` проти ключа релізу, вбудованого в BackDown. Підпис, що не збігається, зупиняє все: файли не були підписані BackDown, або їх змінили після підписання;
2. копіює програму поряд з тією, що у вас є, і перевіряє її SHA-256 проти `SHA256SUMS.txt`;
3. перевіряє, що вона новіша за версію, яка у вас є;
4. встановлює її так само, як це робить [оновлення через мережу](/docs/guides/settings-and-licences/#updates): перевіряє, що нова програма запускається і називає очікувану версію, зберігає стару у вашій теці даних (`previous/`), обмінює їх місцями, перезапускає демон, і повертає стару назад, якщо нова не запускається.

Це взагалі не робить жодного мережевого запиту. `backdown update --rollback` повертає версію, яку було замінено.

| Option | What it does |
|---|---|
| `--check` | Лише перевірити теку; нічого не змінювати |
| `--allow-unsigned` | Продовжити без `SHA256SUMS.txt.sig`. Контрольні суми тоді показують лише те, що файли дійшли цілими, а не те, що вони від BackDown: хто міг змінити файли, міг і змінити `SHA256SUMS.txt`. Підпис, що є, але не збігається, однаково відхиляється |
| `--allow-older` | Встановити, навіть якщо вона не новіша за версію, яка у вас є (чи та сама) |

Релізи до v1.0.0-beta.4 не були підписані, тож оновлення з одного з них потребує `--allow-unsigned`.

### Встановлено з пакунка

Копія BackDown, встановлена з пакунка, не може замінити саму себе, тож `backdown update --from` замість цього каже, що робити з принесеними файлами, і спершу перевіряє пакунок проти `SHA256SUMS.txt`. Для `.deb`:

```text
backdown: /usr/bin/backdown is managed by your package manager (it came in a .deb or another package): install the new package from the carried files instead: sudo apt install /media/usb/backdown-io_1.0.0_amd64.deb (backdown-io_1.0.0_amd64.deb matches SHA256SUMS.txt, which is signed with BackDown's release key)
```

Для AppImage використайте новий AppImage замість старого; для BackDown.app відкрийте новий `.dmg`; для `.msi` запустіть новий `.msi`, який оновлює той, що у вас є.

## Перевірити підпис без BackDown

Щоб перевірити реліз на комп'ютері без BackDown, або як другу думку, підпис може перевірити OpenSSL 3. Це ключ релізу BackDown (`b88db195d0c4084e5ae25f07fbf2f046939021deca58dce2d4e2591875d9e2a8`) як файл публічного ключа:

```sh
cat > backdown-release-key.pem <<'KEY'
-----BEGIN PUBLIC KEY-----
MCowBQYDK2VwAyEAuI2xldDECE5a4l8H+/LwRpOQId7KWNzi1OJZGHXZ4qg=
-----END PUBLIC KEY-----
KEY
# the signature is base64url, one line: turn it into raw bytes
(tr '_-' '/+' < SHA256SUMS.txt.sig | tr -d '\n'; echo '==') | base64 -d > SHA256SUMS.txt.sig.bin
openssl pkeyutl -verify -pubin -inkey backdown-release-key.pem -rawin -in SHA256SUMS.txt -sigfile SHA256SUMS.txt.sig.bin
sha256sum -c --ignore-missing SHA256SUMS.txt            # macOS: shasum -a 256 -c --ignore-missing SHA256SUMS.txt
```

`Signature Verified Successfully`, і `OK` для кожного файлу, який у вас є, означають, що файли — ті, які випустив BackDown.

## Що може досягти мережі

Ніщо в BackDown не досягає мережі, якщо ви не попросили. Це єдині речі, які можуть:

- **Перевірки оновлень і оновлення через мережу**: **Check now**, **Look for updates once a day**, **Update now**, і `backdown update` без `--from`. Вони питають список релізів BackDown на GitHub і завантажують з нього.
- **Інші комп'ютери**: додавання, тестування, синхронізація й перегляд комп'ютера через ssh, і синхронізація доданих комп'ютерів демоном кожні кілька годин.
- **`--host`**: слухання на адресі, іншій за 127.0.0.1, щоб інші пристрої могли дивитися.
- **Відкриття файлу в іншому застосунку** з браузера: той застосунок може вийти в мережу сам.

Ліцензія, індексація, хешування, перевірка, звіт, дзеркала, Rescue, стан дисків і сповіщення — ніколи. (Диск чи дзеркало на мережевій спільній теці читається й записується через систему, як будь-яка інша тека.)
