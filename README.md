# Stackyard: ściąga

Przenośny zestaw do lokalnego developmentu na Windows (Apache, PHP, MySQL, Node, Composer, Git,
Python), lekki zamiennik Laragona. Wszystko, łącznie z ustawieniami, mieszka w tym folderze.

Na co dzień: uruchom `Stackyard.exe`. Zamknięcie okna chowa program do zasobnika (ikona przy zegarze),
a usługi dalej działają. Program kończy przycisk **Zakończ**.

## Polecenia

Wszystkie uruchamiasz w `D:\SERVER\app` (`cd D:\SERVER\app`). W innym folderze, np. w `www`, npm zgłosi
`Could not read package.json`. Wyjątek: release zrobisz z dowolnego miejsca przez
`D:\SERVER\release.cmd` (przyjmuje te same opcje, np. `D:\SERVER\release.cmd -AllVersions`).

| Polecenie | Co robi |
|---|---|
| `npm run release` | Paczka dla kolegi: `D:\SERVER\release\Stackyard-<wersja>-win-x64.zip` (opis niżej) |
| `npm run tauri build` | Buduje exe do `app\src-tauri\target\release\stackyard.exe`. Do testów skopiuj go do `D:\SERVER\Stackyard.exe` przy zamkniętym Stackyard |
| `npm run tauri dev` | Wersja deweloperska z przeładowaniem UI na żywo |
| `npx vite` | Sam podgląd UI w przeglądarce, z atrapą backendu: http://localhost:1420 |
| `npx tsc --noEmit` | Sprawdzenie typów TypeScript |
| `cd src-tauri; cargo clippy --all-targets; cargo test` | Lint i testy Rusta |

Do budowania potrzebne są Rust (rustup), MSVC Build Tools (C++) i Node. Jeśli w PowerShell pojawia się
`cargo: program not found`, dopisz cargo do PATH w tej samej sesji:
`$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"` (`npm run release` robi to sam).

## Test na czystym Windowsie

```
npm run sandbox
```

Otwiera Windows Sandbox z najnowszym zipem z `release\`, rozpakowuje go do `C:\Stackyard` i uruchamia.
Sandbox to czysty Windows (bez VC++ Redistributable, bez Twoich ustawień), a po zamknięciu okna
wszystko znika. Wymaga włączonej funkcji „Piaskownica systemu Windows” (Windows Pro).

## Release (paczka do udostępnienia)

```
npm run release                   # exe + wybrana wersja każdego narzędzia
npm run release -- -AllVersions   # wszystkie zainstalowane wersje (np. obie wersje PHP)
npm run release -- -NoBin         # samo exe, pusty bin\ z instrukcją
npm run release -- -KeepFolder    # zostawia też rozpakowany folder obok zipa
```

Skrypt sprawdza zgodność wersji, uruchamia TypeScript i testy, buduje exe i składa paczkę. Na końcu
tworzy zip i plik `.sha256`.

W zipie jest `README.txt` dla odbiorcy, generowany z `app\scripts\README-release.txt`: instalacja,
SmartScreen, przejście z Laragona, PATH i logi.

Do folderów Apache, PHP i MySQL w paczce trafia też runtime Visual C++ (`vcruntime140.dll`,
`vcruntime140_1.dll`, `msvcp140.dll`), więc u odbiorcy nie trzeba nic instalować. Pliki są kopiowane
z `C:\Windows\System32` Twojego komputera, dlatego trzymaj VC++ Redistributable aktualny. Skrypt
przerwie release, jeśli wersja będzie za stara.

**Do paczki nie trafia** nic z Twojego komputera: `www\`, `data\` (bazy MySQL), `config\`, `logs\`,
`tmp\`, `backup\`, a z `bin\` także wygenerowane `php.ini`, logi Apache itp. Na końcu skrypt przeszukuje
paczkę pod kątem ścieżek `D:\SERVER`, Twojego folderu użytkownika, Laragona i danych z gita. Jeśli coś
znajdzie, przerywa i pokazuje, co i gdzie.

**Nowa wersja Stackyard:** zmień numer w trzech plikach na ten sam: `app\package.json`,
`app\src-tauri\Cargo.toml` i `app\src-tauri\tauri.conf.json`. Przy niezgodności release się nie uruchomi.

## Nowa wersja narzędzia

**PHP, Node.js i Composer** aktualizują się z programu: zakładka **Narzędzia i PATH** →
**Sprawdź aktualizacje** → **Pobierz** → **Użyj …**. Nowa wersja ląduje obok starej.

**Pozostałe (i każde ręcznie):** rozpakuj do `bin\<narzędzie>\<nazwa-wersji>\`, kliknij **Odśwież** i
wybierz wersję z listy. Żadnej konfiguracji nie trzeba. Stackyard sam szuka najważniejszego pliku
narzędzia:

| Narzędzie | Folder | Musi zawierać | Skąd i uwagi |
|---|---|---|---|
| Apache | `bin\apache\httpd-2.4.x-…-Win64-VS18\` | `bin\httpd.exe` | apachelounge.com/download. Zip ma w środku folder `Apache24`: wypakuj **jego zawartość** do folderu wersji |
| PHP | `bin\php\php-8.x.x-Win32-vs17-x64\` | `php.exe` | windows.php.net, wersja **Thread Safe x64** (inną Apache nie uruchomi) |
| MySQL | `bin\mysql\mysql-8.4.x-winx64\` | `bin\mysqld.exe` | dev.mysql.com, „ZIP Archive”, bez „Debug Binaries”. Po rozpakowaniu można usunąć zbędne ~950 MB: `*.pdb`, `*.lib`, `bin\*-debug.dll`, `include\`, `lib\plugin\debug\`, `lib\mecab\` i `lib\plugin\libpluginmecab.dll` |
| Node.js | `bin\nodejs\node-v26.x.x-win-x64\` | `node.exe` | nodejs.org, zip dla Windows x64 |
| Python | `bin\python\python-3.x\` | `python.exe` | |
| Git | `bin\git\` (bez podfolderu wersji) | `cmd\git.exe` | PortableGit z GitHuba. Wypakuj do pustego folderu: `PortableGit-….7z.exe -o"D:\SERVER\tmp\git-new" -y`, potem podmień nim `bin\git` |
| Composer | `bin\composer\` (bez podfolderu wersji) | `composer.phar` | Podmień plik (program robi to sam przy aktualizacji) |

**Uwagi:**
- **Nazwa folderu** to nazwa wersji na liście. Najnowsza (sortowanie naturalne, 8.4.13 > 8.4.3) jest
  wybierana automatycznie, dopóki nie wybierzesz innej.
- **PHP w nowej serii** (np. 8.4, gdy masz 8.3 i 8.5) nie ma sterowników SQL Server (`sqlsrv`,
  `pdo_sqlsrv`). Pobierz je od Microsoftu dla tej wersji (Thread Safe x64) i wrzuć do jej `ext\`. W obrębie
  tej samej serii (np. 8.3.36 → 8.3.40) Stackyard kopiuje je sam przy pobieraniu.
- **MySQL:** baza leży w `data\mysql-<major.minor>\`, np. `data\mysql-8.4`. Nowa wersja z tej samej serii
  (8.4.3 → 8.4.13) używa tej samej bazy i przy pierwszym starcie sama ją aktualizuje. **Przed taką
  zmianą skopiuj `data\mysql-8.4`.** Inna seria (np. 9.x) zaczyna z nową, pustą bazą.
- **Działająca usługa** (Apache, MySQL) po zmianie wersji sama się restartuje.

**Usuwanie starej wersji:** zatrzymaj usługę, usuń jej folder z `bin\…` i kliknij **Odśwież**. Jeśli to
była wybrana wersja, Stackyard przejdzie na najnowszą. Narzędzie dodane do PATH przestaw w programie
(przełącznik PATH), żeby PATH nie wskazywał na usunięty folder.

## Gdzie co jest

| Ścieżka | Zawartość |
|---|---|
| `bin\` | narzędzia |
| `www\` | Twoje projekty (DocumentRoot dla `http://localhost`) |
| `data\` | bazy MySQL |
| `config\settings.json` | ustawienia (porty, wybrane wersje, motyw, język, wirtualne hosty) |
| `config\apache\`, `config\php\`, `config\mysql\` | wygenerowane konfiguracje. Własne dodatki wpisuj do `conf.d\` w tych folderach: Stackyard ich nie nadpisuje |
| `config\backups\` | kopie PATH sprzed każdej zmiany |
| `logs\` | logi Stackyard, Apache, PHP i MySQL |
| `backup\` | ręczne kopie (np. bazy sprzed aktualizacji MySQL) |
| `app\` | kod źródłowy |

Porty zmienisz w zakładce **Ustawienia → Porty**. Działająca usługa sama się zrestartuje na nowym porcie.

**Ustawienia są chronione:** każdy zapis jest atomowy, a poprzednia wersja zostaje jako
`config\settings.json.bak`. Jeśli ręczna edycja zepsuje plik, Stackyard odłoży go jako
`settings.json.broken`, przywróci ostatnią dobrą wersję i pokaże o tym komunikat.

**MySQL** zatrzymuje się kontem `stackyard_shutdown`, które ma tylko uprawnienie SHUTDOWN. Hasło jest
w `config\mysql\shutdown.cnf`. Dzięki temu możesz ustawić hasło roota, a zatrzymywanie dalej będzie
czyste. Nie usuwaj tego konta.
