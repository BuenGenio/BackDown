//! `backdown desktop install|uninstall`: BackDown among your applications, opening `backdown app`.
//!
//! - Linux: a desktop entry in ~/.local/share/applications and the icon in the hicolor theme
//!   (every size, and scalable); with --autostart, an XDG autostart entry that starts the daemon at
//!   login (`backdown app --no-window`).
//! - macOS: ~/Applications/BackDown.app, a small bundle (Info.plist, an .icns icon, a launcher that
//!   runs this backdown with `app`).
//! - Windows: a Start menu shortcut (a .lnk made by PowerShell through WScript.Shell) with the
//!   icon; with --autostart, one in the Startup folder too.
//!
//! The same generators write what installers need (`backdown desktop files`, hidden, used by
//! scripts/package-local.sh): the .deb's and AppImage's files, BackDown.app's Info.plist and icon,
//! and the Windows installer scripts and MSI source. They're plain text and bytes, so a Linux build
//! makes them for every platform. The icons are rendered from the brand mark by
//! scripts/make-icons.py and compiled in.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};

use crate::locations;

pub const APP_ID: &str = "io.backdown.BackDown";
const SUMMARY: &str = "See what's on your drives, what's duplicated, and what's safe to wipe";

macro_rules! icons {
    ($($n:literal => $f:literal),* $(,)?) => { &[$(($n, include_bytes!(concat!("../assets/icons/", $f)) as &[u8])),*] };
}

/// The tile for Linux and Windows, by size in pixels.
pub const ICONS: &[(u32, &[u8])] = icons! {
    16 => "backdown-16.png", 24 => "backdown-24.png", 32 => "backdown-32.png", 48 => "backdown-48.png",
    64 => "backdown-64.png", 128 => "backdown-128.png", 256 => "backdown-256.png", 512 => "backdown-512.png",
};
/// The tile on Apple's icon grid, for the .icns.
pub const MAC_ICONS: &[(u32, &[u8])] = icons! {
    16 => "backdown-macos-16.png", 32 => "backdown-macos-32.png", 64 => "backdown-macos-64.png",
    128 => "backdown-macos-128.png", 256 => "backdown-macos-256.png", 512 => "backdown-macos-512.png",
    1024 => "backdown-macos-1024.png",
};
pub const SVG_ICON: &str = include_str!("../assets/icons/backdown.svg");

fn icon(set: &[(u32, &'static [u8])], size: u32) -> &'static [u8] {
    set.iter()
        .find(|(n, _)| *n == size)
        .map(|(_, b)| *b)
        .expect("icon size compiled in")
}

#[derive(clap::Subcommand)]
pub enum DesktopCmd {
    /// Add BackDown to your applications: the menu and icons (Linux), ~/Applications/BackDown.app
    /// (macOS), or the Start menu (Windows). It opens `backdown app`
    Install {
        /// Also start BackDown's background service when you log in (on macOS, the login item
        /// `backdown service install` makes)
        #[arg(long)]
        autostart: bool,
    },
    /// Remove what `desktop install` added (your index, settings and licence stay)
    Uninstall,
    /// Write the desktop files an installer needs (for scripts/package-local.sh)
    #[command(hide = true)]
    Files {
        /// linux (a .deb's files), appimage (an AppDir's), macos (BackDown.app's) or windows
        #[arg(long)]
        platform: String,
        /// the folder to write them in
        #[arg(long)]
        out: PathBuf,
        /// where the package puts the binary (linux: /usr/bin/backdown)
        #[arg(long)]
        exe: Option<String>,
        /// the version to write (default: this binary's)
        #[arg(long)]
        version: Option<String>,
    },
}

pub fn run(cmd: DesktopCmd) -> Result<ExitCode> {
    match cmd {
        DesktopCmd::Install { autostart } => install(autostart),
        DesktopCmd::Uninstall => uninstall(),
        DesktopCmd::Files {
            platform,
            out,
            exe,
            version,
        } => {
            let version = version.unwrap_or_else(|| env!("CARGO_PKG_VERSION").into());
            let files = package_files(&platform, exe.as_deref(), &version)?;
            for f in &files {
                write_file(&out.join(&f.path), &f.data, f.mode)?;
                println!("{}", f.path);
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

// ---------- generated files

/// One argument of a desktop entry's Exec, quoted and escaped as the Desktop Entry spec says (then
/// escaped again as a string value).
pub fn exec_arg(s: &str) -> String {
    let s = s.replace('%', "%%");
    let reserved = |c: char| " \t\n\"'\\><~|&;$*?#()`".contains(c);
    if !s.is_empty() && !s.chars().any(reserved) {
        return s;
    }
    let mut q = String::from("\"");
    for c in s.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            q.push('\\');
        }
        q.push(c);
    }
    q.push('"');
    q.replace('\\', "\\\\")
}

fn string_value(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
}

/// The applications-menu entry. `exe` is how to run BackDown (a path, or `backdown` in an AppImage).
pub fn desktop_entry(exe: &str) -> String {
    let try_exec = if exe.contains('/') {
        format!("TryExec={}\n", string_value(exe))
    } else {
        String::new()
    };
    format!(
        "[Desktop Entry]
Type=Application
Version=1.5
Name=BackDown
GenericName=Drive inventory
Comment={SUMMARY}
Exec={} app
{try_exec}Icon={APP_ID}
Terminal=false
Categories=Utility;Archiving;
Keywords=backup;duplicates;dedup;drives;disks;wipe;archive;
StartupNotify=true
StartupWMClass=BackDown
",
        exec_arg(exe)
    )
}

/// Started at login: keeps the daemon running, opens no window.
pub fn autostart_entry(exe: &str) -> String {
    format!(
        "[Desktop Entry]
Type=Application
Name=BackDown
Comment=Keeps BackDown's background service running, to rescan your drives
Exec={} app --no-window
Icon={APP_ID}
Terminal=false
NoDisplay=true
X-GNOME-Autostart-enabled=true
",
        exec_arg(exe)
    )
}

/// AppStream metadata, for software centres (and appimagetool's checks).
pub fn metainfo(version: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<component type="desktop-application">
  <id>{APP_ID}</id>
  <metadata_license>CC0-1.0</metadata_license>
  <project_license>LicenseRef-proprietary</project_license>
  <name>BackDown</name>
  <summary>{SUMMARY}</summary>
  <description>
    <p>BackDown indexes your drives, finds what is duplicated with a SHA-256 hash behind every claim, recognises what things are (mail stores, backups, phones, systems, repositories, photo libraries) and tells you which drives are safe to wipe. It only ever reads your drives.</p>
    <p>It runs on this computer only: a background service keeps drives up to date, and its window shows folders as they were at any scan.</p>
  </description>
  <launchable type="desktop-id">{APP_ID}.desktop</launchable>
  <url type="homepage">https://backdown.io</url>
  <developer id="io.backdown">
    <name>backdown.io</name>
  </developer>
  <provides>
    <binary>backdown</binary>
  </provides>
  <categories>
    <category>Utility</category>
  </categories>
  <content_rating type="oars-1.1"/>
  <releases>
    <release version="{version}"/>
  </releases>
</component>
"#
    )
}

/// The numeric x.y.z macOS wants in a bundle's version keys (the pre-release part is dropped).
fn numeric_version(v: &str) -> String {
    let core = v.split(['-', '+']).next().unwrap_or(v);
    let parts: Vec<&str> = core.split('.').take(3).collect();
    if parts.len() == 3 && parts.iter().all(|p| p.parse::<u32>().is_ok()) {
        parts.join(".")
    } else {
        "0.0.0".into()
    }
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// BackDown.app's Info.plist; `executable` is the file in Contents/MacOS that LaunchServices runs.
pub fn info_plist(version: &str, executable: &str) -> String {
    let n = numeric_version(version);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleDevelopmentRegion</key>
	<string>en</string>
	<key>CFBundleDisplayName</key>
	<string>BackDown</string>
	<key>CFBundleExecutable</key>
	<string>{}</string>
	<key>CFBundleIconFile</key>
	<string>BackDown</string>
	<key>CFBundleIdentifier</key>
	<string>{APP_ID}</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleName</key>
	<string>BackDown</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>{n}</string>
	<key>CFBundleVersion</key>
	<string>{n}</string>
	<key>BackDownVersion</key>
	<string>{}</string>
	<key>LSApplicationCategoryType</key>
	<string>public.app-category.utilities</string>
	<key>LSMinimumSystemVersion</key>
	<string>11.0</string>
	<key>NSHighResolutionCapable</key>
	<true/>
	<key>NSHumanReadableCopyright</key>
	<string>backdown.io</string>
</dict>
</plist>
"#,
        xml(executable),
        xml(version)
    )
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The launcher in ~/Applications/BackDown.app: runs this binary, wherever it is, as the app.
pub fn mac_launcher(exe: &Path) -> String {
    format!(
        "#!/bin/sh\n# BackDown's launcher, written by `backdown desktop install`\nexec {} app \"$@\"\n",
        sh_quote(&exe.display().to_string())
    )
}

/// An .icns from PNGs: every size macOS looks for, at 1x and 2x.
pub fn icns() -> Vec<u8> {
    let entries: &[(&[u8; 4], u32)] = &[
        (b"icp4", 16),
        (b"icp5", 32),
        (b"icp6", 64),
        (b"ic07", 128),
        (b"ic08", 256),
        (b"ic09", 512),
        (b"ic10", 1024),
        (b"ic11", 32),
        (b"ic12", 64),
        (b"ic13", 256),
        (b"ic14", 512),
    ];
    let mut body = Vec::new();
    for (kind, size) in entries {
        let png = icon(MAC_ICONS, *size);
        body.extend_from_slice(*kind);
        body.extend_from_slice(&(png.len() as u32 + 8).to_be_bytes());
        body.extend_from_slice(png);
    }
    let mut out = b"icns".to_vec();
    out.extend_from_slice(&(body.len() as u32 + 8).to_be_bytes());
    out.extend(body);
    out
}

/// A Windows .ico holding PNGs (Windows Vista and later read those).
pub fn ico() -> Vec<u8> {
    let sizes = [16u32, 24, 32, 48, 64, 256];
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len();
    for s in sizes {
        let png = icon(ICONS, s);
        let dim = if s >= 256 { 0 } else { s as u8 };
        out.extend_from_slice(&[dim, dim, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes()); // planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(png.len() as u32).to_le_bytes());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += png.len();
    }
    for s in sizes {
        out.extend_from_slice(icon(ICONS, s));
    }
    out
}

fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// PowerShell that makes a shortcut with WScript.Shell (minimised: the console of a console
/// program then doesn't flash up).
/// PowerShell that makes a shortcut with WScript.Shell. backdown.exe is a console program, so the
/// shortcut runs it through `conhost.exe --headless` (as the scheduled task does): no console
/// window flashes up. Minimised too, in case a Windows too old for --headless shows one.
pub fn lnk_script(lnk: &Path, exe: &Path, args: &str, icon: &Path) -> String {
    let dir = exe.parent().unwrap_or(Path::new("."));
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    format!(
        "$s = (New-Object -ComObject WScript.Shell).CreateShortcut({});\n\
         $s.TargetPath = {};\n$s.Arguments = {};\n$s.WorkingDirectory = {};\n\
         $s.IconLocation = {};\n$s.Description = {};\n$s.WindowStyle = 7;\n$s.Save()\n",
        ps_quote(&lnk.display().to_string()),
        ps_quote(&format!(r"{root}\System32\conhost.exe")),
        ps_quote(&format!("--headless \"{}\" {args}", exe.display())),
        ps_quote(&dir.display().to_string()),
        ps_quote(&format!("{},0", icon.display())),
        ps_quote("BackDown: see what's on your drives and what's duplicated"),
    )
}

fn crlf(s: &str) -> Vec<u8> {
    s.replace("\r\n", "\n").replace('\n', "\r\n").into_bytes()
}

/// The per-user Windows installer: copies BackDown to %LOCALAPPDATA%\Programs\BackDown, puts it on
/// your PATH, lets BackDown make its Start menu entry, and lists it under Settings > Apps.
/// (ASCII only: Windows PowerShell reads a script without a BOM as the ANSI code page.)
pub fn windows_install_ps1(version: &str) -> String {
    format!(
        r#"# Installs BackDown {version} for this Windows user: no administrator rights needed.
# Double-click "Install BackDown.cmd", or run:
#   powershell -ExecutionPolicy Bypass -File install.ps1 [-Autostart] [-NoLaunch]
param([switch]$Autostart, [switch]$NoLaunch)
$ErrorActionPreference = 'Stop'
$version = '{version}'
$src = Split-Path -Parent $MyInvocation.MyCommand.Path
$dest = Join-Path $env:LOCALAPPDATA 'Programs\BackDown'
$exe = Join-Path $dest 'backdown.exe'
New-Item -ItemType Directory -Force -Path $dest | Out-Null
# a running BackDown keeps its .exe open, but Windows lets it be renamed out of the way
if (Test-Path $exe) {{
  Remove-Item "$exe.old" -Force -ErrorAction SilentlyContinue
  Rename-Item -Path $exe -NewName 'backdown.exe.old'
}}
Copy-Item -Path (Join-Path $src 'backdown.exe') -Destination $exe -Force
foreach ($f in 'README.txt', 'THIRD-PARTY-NOTICES.txt', 'backdown.ico', 'uninstall.ps1', 'Uninstall BackDown.cmd') {{
  $p = Join-Path $src $f
  if (Test-Path $p) {{ Copy-Item -Path $p -Destination $dest -Force }}
}}
# the backdown command, in terminals opened from now on
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$parts = @(($userPath -split ';') | Where-Object {{ $_ }})
if ($parts -notcontains $dest) {{
  [Environment]::SetEnvironmentVariable('Path', (($parts + $dest) -join ';'), 'User')
}}
# the Start menu entry (and, if asked, starting at login): BackDown makes them itself
$a = @('desktop', 'install')
if ($Autostart) {{ $a += '--autostart' }}
& $exe @a
if ($LASTEXITCODE -ne 0) {{ throw 'backdown desktop install failed' }}
# Settings > Apps lists it, with its uninstaller
$key = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\BackDown'
New-Item -Path $key -Force | Out-Null
$values = @{{
  DisplayName = 'BackDown'; DisplayVersion = $version; Publisher = 'backdown.io'; URLInfoAbout = 'https://backdown.io'
  DisplayIcon = (Join-Path $dest 'backdown.ico'); InstallLocation = $dest
  UninstallString = "powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$dest\uninstall.ps1`""
}}
foreach ($k in $values.Keys) {{ Set-ItemProperty -Path $key -Name $k -Value $values[$k] }}
foreach ($k in 'NoModify', 'NoRepair') {{ Set-ItemProperty -Path $key -Name $k -Value 1 -Type DWord }}
$size = [int]((Get-ChildItem -Path $dest -File | Measure-Object -Property Length -Sum).Sum / 1024)
Set-ItemProperty -Path $key -Name EstimatedSize -Value $size -Type DWord
Write-Host "BackDown $version is installed in $dest."
Write-Host 'Open it from the Start menu. In a new terminal, the backdown command works too.'
# open it now, through a hidden console like the Start menu entry
if (-not $NoLaunch) {{ Start-Process -FilePath (Join-Path $env:SystemRoot 'System32\conhost.exe') -ArgumentList "--headless `"$exe`" app" }}
"#
    )
}

pub fn windows_uninstall_ps1() -> String {
    r#"# Removes BackDown for this Windows user. Your index, settings and licence stay in
# %LOCALAPPDATA%\BackDown: delete that folder to remove them too.
$ErrorActionPreference = 'Continue'
$dest = Join-Path $env:LOCALAPPDATA 'Programs\BackDown'
$exe = Join-Path $dest 'backdown.exe'
if (Test-Path $exe) { & $exe desktop uninstall }
# a BackDown running in the background keeps its .exe open: stop it (its work resumes next time)
Get-Process -Name backdown -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $exe } | Stop-Process -Force
Start-Sleep -Milliseconds 500
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if ($userPath) {
  $kept = @(($userPath -split ';') | Where-Object { $_ -and ($_ -ne $dest) })
  [Environment]::SetEnvironmentVariable('Path', ($kept -join ';'), 'User')
}
Remove-Item -Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\BackDown' -Recurse -Force -ErrorAction SilentlyContinue
Remove-Item -Path $dest -Recurse -Force -ErrorAction SilentlyContinue
Write-Host 'BackDown is removed. Your index, settings and licence are still in' (Join-Path $env:LOCALAPPDATA 'BackDown')
"#
    .into()
}

pub fn windows_install_cmd() -> String {
    "@echo off\nrem Installs BackDown for you, no administrator rights needed: double-click this file.\n\
     powershell.exe -NoProfile -ExecutionPolicy Bypass -File \"%~dp0install.ps1\" %*\nif errorlevel 1 pause\n"
        .into()
}

pub fn windows_uninstall_cmd() -> String {
    // one line: the script deletes the folder this file is in, and cmd reads a batch file as it goes
    "@echo off\nrem Removes BackDown for you; your index and settings stay.\n\
     powershell.exe -NoProfile -ExecutionPolicy Bypass -File \"%~dp0uninstall.ps1\" & timeout /t 5 >nul & exit /b\n"
        .into()
}

/// MSI's ProductVersion is major.minor.build with build < 65536, and must grow with each release
/// for upgrades: build = patch × 1000 + (alpha 100, beta 200, rc 300, release 900) + the
/// pre-release number. 0.2.0-alpha.4 → 0.2.104, 0.2.0-rc.1 → 0.2.301, 0.2.0 → 0.2.900,
/// 0.2.1 → 0.2.1900.
pub fn msi_version(v: &str) -> Result<String> {
    let ver = crate::update::Version::parse(v).with_context(|| format!("not a version: {v}"))?;
    let stage = match ver.pre.first() {
        None => 900,
        Some(crate::update::Ident::Text(t)) if t.starts_with("alpha") => 100,
        Some(crate::update::Ident::Text(t)) if t.starts_with("beta") => 200,
        Some(crate::update::Ident::Text(t)) if t.starts_with("rc") => 300,
        Some(_) => 0,
    };
    let n = ver
        .pre
        .iter()
        .find_map(|i| match i {
            crate::update::Ident::Num(n) => Some(*n),
            crate::update::Ident::Text(_) => None,
        })
        .unwrap_or(0)
        .min(99);
    let build = ver.patch * 1000 + stage + n;
    if ver.major > 255 || ver.minor > 255 || build > 65_535 {
        bail!("{v} doesn't fit an MSI version");
    }
    Ok(format!("{}.{}.{build}", ver.major, ver.minor))
}

/// Never change: Windows Installer knows BackDown's installs by it.
const UPGRADE_CODE: &str = "B35A3C05-BE8E-44AC-829B-CCF87558062C";
const PROGRAM_COMPONENT: &str = "EFC2C713-F5EF-4FCE-A39B-8B3DE89201DD";
const MENU_COMPONENT: &str = "5C1DE715-77C7-4F62-A776-85DDA2695F2A";

/// The per-user MSI's source, for msitools' wixl: %LOCALAPPDATA%\Programs\BackDown, a Start menu
/// shortcut, the PATH entry; no administrator rights.
pub fn wxs(version: &str) -> Result<String> {
    let msi = msi_version(version)?;
    Ok(format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<!-- BackDown {version} for one Windows user, no administrator rights needed. Written by
     BackDown's own "desktop files" command; built by scripts/package-local.sh with wixl. -->
<Wix xmlns="http://schemas.microsoft.com/wix/2006/wi">
  <Product Id="*" Name="BackDown" Language="1033" Version="{msi}" Manufacturer="backdown.io" UpgradeCode="{UPGRADE_CODE}">
    <Package InstallerVersion="500" Compressed="yes" InstallScope="perUser"
             Description="BackDown {version}" Comments="{summary}" Manufacturer="backdown.io"/>
    <MajorUpgrade AllowSameVersionUpgrades="yes" DowngradeErrorMessage="A newer BackDown is installed already."/>
    <Media Id="1" Cabinet="backdown.cab" EmbedCab="yes"/>
    <Icon Id="BackDownIcon" SourceFile="backdown.ico"/>
    <Property Id="ARPPRODUCTICON" Value="BackDownIcon"/>
    <Property Id="ARPURLINFOABOUT" Value="https://backdown.io"/>
    <Property Id="ARPNOMODIFY" Value="1"/>
    <Property Id="ARPNOREPAIR" Value="1"/>
    <Directory Id="TARGETDIR" Name="SourceDir">
      <Directory Id="LocalAppDataFolder">
        <Directory Id="ProgramsDir" Name="Programs">
          <Directory Id="INSTALLDIR" Name="BackDown">
            <Component Id="Program" Guid="{PROGRAM_COMPONENT}" Win64="yes">
              <File Id="BackdownExe" Name="backdown.exe" Source="backdown.exe"/>
              <File Id="ReadMe" Name="README.txt" Source="README.txt"/>
              <File Id="Notices" Name="THIRD-PARTY-NOTICES.txt" Source="THIRD-PARTY-NOTICES.txt"/>
              <File Id="IconFile" Name="backdown.ico" Source="backdown.ico"/>
              <RegistryValue Root="HKCU" Key="Software\BackDown" Name="program" Type="integer" Value="1" KeyPath="yes"/>
              <Environment Id="UserPath" Name="PATH" Value="[INSTALLDIR]" Action="set" Part="last" System="no" Permanent="no"/>
              <RemoveFolder Id="RemoveInstallDir" On="uninstall"/>
            </Component>
          </Directory>
        </Directory>
      </Directory>
      <Directory Id="ProgramMenuFolder">
        <Component Id="StartMenu" Guid="{MENU_COMPONENT}" Win64="yes">
          <!-- through conhost's hidden console, like the scheduled task: no console window flashes up -->
          <Shortcut Id="StartMenuShortcut" Name="BackDown" Description="{summary}" Target="[System64Folder]conhost.exe"
                    Arguments="--headless &quot;[INSTALLDIR]backdown.exe&quot; app" WorkingDirectory="INSTALLDIR"
                    Icon="BackDownIcon"/>
          <RegistryValue Root="HKCU" Key="Software\BackDown" Name="startmenu" Type="integer" Value="1" KeyPath="yes"/>
        </Component>
      </Directory>
    </Directory>
    <Feature Id="Main" Level="1" Title="BackDown">
      <ComponentRef Id="Program"/>
      <ComponentRef Id="StartMenu"/>
    </Feature>
  </Product>
</Wix>
"#,
        summary = xml(SUMMARY)
    ))
}

/// The AppImage's entry point: no arguments opens the window; otherwise it's the backdown command.
pub fn apprun() -> String {
    "#!/bin/sh\n# BackDown AppImage: with no arguments it opens BackDown's window; with arguments it is\n\
     # the backdown command (backdown index, backdown check, ...)\n\
     HERE=$(dirname \"$(readlink -f \"$0\")\")\n[ $# -eq 0 ] && set -- app\nexec \"$HERE/usr/bin/backdown\" \"$@\"\n"
        .into()
}

pub struct PackageFile {
    pub path: String,
    pub data: Vec<u8>,
    pub mode: u32,
}

fn pf(path: impl Into<String>, data: impl Into<Vec<u8>>, mode: u32) -> PackageFile {
    PackageFile {
        path: path.into(),
        data: data.into(),
        mode,
    }
}

/// The hicolor icons below `share` (usr/share or ~/.local/share).
fn hicolor(share: &str) -> Vec<PackageFile> {
    let mut v: Vec<PackageFile> = ICONS
        .iter()
        .map(|(n, png)| {
            pf(
                format!("{share}/icons/hicolor/{n}x{n}/apps/{APP_ID}.png"),
                *png,
                0o644,
            )
        })
        .collect();
    v.push(pf(
        format!("{share}/icons/hicolor/scalable/apps/{APP_ID}.svg"),
        SVG_ICON,
        0o644,
    ));
    v
}

/// What an installer for `platform` needs besides the binary, as paths inside the package.
pub fn package_files(platform: &str, exe: Option<&str>, version: &str) -> Result<Vec<PackageFile>> {
    Ok(match platform {
        "linux" => {
            let exe = exe.unwrap_or("/usr/bin/backdown");
            let mut v = vec![
                pf(
                    format!("usr/share/applications/{APP_ID}.desktop"),
                    desktop_entry(exe),
                    0o644,
                ),
                pf(
                    format!("usr/share/metainfo/{APP_ID}.metainfo.xml"),
                    metainfo(version),
                    0o644,
                ),
                // a user unit anyone can turn on (systemctl --user enable --now backdown); not on
                // for everyone
                pf(
                    "usr/lib/systemd/user/backdown.service",
                    crate::service::user_unit(Path::new(exe), ""),
                    0o644,
                ),
            ];
            v.extend(hicolor("usr/share"));
            v
        }
        "appimage" => {
            let mut v = vec![
                pf("AppRun", apprun(), 0o755),
                pf(
                    format!("{APP_ID}.desktop"),
                    desktop_entry("backdown"),
                    0o644,
                ),
                pf(format!("{APP_ID}.png"), icon(ICONS, 256), 0o644),
                pf(".DirIcon", icon(ICONS, 256), 0o644),
                pf(
                    format!("usr/share/applications/{APP_ID}.desktop"),
                    desktop_entry("backdown"),
                    0o644,
                ),
                pf(
                    format!("usr/share/metainfo/{APP_ID}.appdata.xml"),
                    metainfo(version),
                    0o644,
                ),
            ];
            v.extend(hicolor("usr/share"));
            v
        }
        "macos" => vec![
            pf(
                "BackDown.app/Contents/Info.plist",
                info_plist(version, "backdown"),
                0o644,
            ),
            pf("BackDown.app/Contents/PkgInfo", "APPL????", 0o644),
            pf(
                "BackDown.app/Contents/Resources/BackDown.icns",
                icns(),
                0o644,
            ),
        ],
        "windows" => vec![
            pf("install.ps1", crlf(&windows_install_ps1(version)), 0o644),
            pf("uninstall.ps1", crlf(&windows_uninstall_ps1()), 0o644),
            pf("Install BackDown.cmd", crlf(&windows_install_cmd()), 0o644),
            pf(
                "Uninstall BackDown.cmd",
                crlf(&windows_uninstall_cmd()),
                0o644,
            ),
            pf("backdown.ico", ico(), 0o644),
            pf("backdown.wxs", wxs(version)?, 0o644),
        ],
        p => bail!("--platform is linux, appimage, macos or windows, not {p}"),
    })
}

// ---------- installing for this user

fn write_file(path: &Path, data: &[u8], mode: u32) -> Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).with_context(|| format!("can't create {}", d.display()))?;
    }
    std::fs::write(path, data).with_context(|| format!("can't write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    Ok(())
}

fn xdg(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(fallback)
        })
}

/// Where `desktop install` puts things for this user on Linux: (path, contents, mode).
fn linux_files(exe: &Path, autostart: bool) -> Vec<(PathBuf, Vec<u8>, u32)> {
    let data = xdg("XDG_DATA_HOME", ".local/share");
    let exe = exe.display().to_string();
    let mut v = vec![(
        data.join(format!("applications/{APP_ID}.desktop")),
        desktop_entry(&exe).into_bytes(),
        0o644,
    )];
    for f in hicolor("") {
        v.push((data.join(f.path.trim_start_matches('/')), f.data, f.mode));
    }
    if autostart {
        v.push((autostart_path(), autostart_entry(&exe).into_bytes(), 0o644));
    }
    v
}

fn autostart_path() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config").join(format!("autostart/{APP_ID}.desktop"))
}

fn mac_app() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join("Applications/BackDown.app")
}

fn windows_menu() -> PathBuf {
    PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default())
        .join(r"Microsoft\Windows\Start Menu\Programs")
}

fn quietly(c: &mut Command) {
    c.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
}

fn powershell(script: &str) -> Result<()> {
    let mut c = Command::new("powershell");
    c.args([
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-Command",
        script,
    ]);
    let out = c.output().context("PowerShell isn't available")?;
    if !out.status.success() {
        bail!(
            "PowerShell couldn't make the shortcut: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

fn install(autostart: bool) -> Result<ExitCode> {
    let exe = crate::app::self_exe()?;
    if exe.starts_with("/tmp") || exe.starts_with(std::env::temp_dir()) {
        eprintln!(
            "note: {} is in a temporary folder; the menu entry breaks when it's cleaned up. Move backdown somewhere permanent (e.g. ~/.local/bin) first.",
            exe.display()
        );
    }
    if cfg!(target_os = "macos") {
        if exe.to_string_lossy().contains(".app/Contents/MacOS/") {
            let app = exe
                .ancestors()
                .find(|p| p.extension().is_some_and(|e| e == "app"))
                .unwrap_or(&exe);
            println!("BackDown.app is installed already: {}", app.display());
            if autostart {
                return crate::service::install(false, "127.0.0.1", 8787, true, false);
            }
            return Ok(ExitCode::SUCCESS);
        }
        let app = mac_app();
        let c = app.join("Contents");
        write_file(
            &c.join("Info.plist"),
            info_plist(env!("CARGO_PKG_VERSION"), "BackDown").as_bytes(),
            0o644,
        )?;
        write_file(&c.join("PkgInfo"), b"APPL????", 0o644)?;
        write_file(
            &c.join("MacOS/BackDown"),
            mac_launcher(&exe).as_bytes(),
            0o755,
        )?;
        write_file(&c.join("Resources/BackDown.icns"), &icns(), 0o644)?;
        let lsregister = "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";
        if Path::new(lsregister).exists() {
            let mut r = Command::new(lsregister);
            r.args(["-f"]).arg(&app);
            quietly(&mut r);
            let _ = r.status();
        }
        println!("added {} (open it from Launchpad or Finder)", app.display());
        if autostart {
            // the background service at login is the launchd agent `backdown service` makes
            return crate::service::install(false, "127.0.0.1", 8787, true, false);
        }
        return Ok(ExitCode::SUCCESS);
    }
    if cfg!(windows) {
        let ico_path = locations::state_dir().join("backdown.ico");
        locations::ensure_private_dir(&locations::state_dir(), false)?;
        write_file(&ico_path, &ico(), 0o644)?;
        let menu = windows_menu().join("BackDown.lnk");
        powershell(&lnk_script(&menu, &exe, "app", &ico_path))?;
        println!("added BackDown to the Start menu ({})", menu.display());
        if autostart {
            // the background service at logon is the scheduled task `backdown service` makes
            return crate::service::install(false, "127.0.0.1", 8787, true, false);
        }
        return Ok(ExitCode::SUCCESS);
    }
    for (path, data, mode) in linux_files(&exe, autostart) {
        write_file(&path, &data, mode)?;
    }
    let data = xdg("XDG_DATA_HOME", ".local/share");
    for (cmd, args) in [
        ("update-desktop-database", vec![data.join("applications")]),
        (
            "gtk-update-icon-cache",
            vec![
                PathBuf::from("-q"),
                PathBuf::from("-t"),
                data.join("icons/hicolor"),
            ],
        ),
    ] {
        let mut c = Command::new(cmd);
        c.args(&args);
        quietly(&mut c);
        let _ = c.status();
    }
    println!(
        "added BackDown to your applications ({})",
        data.join(format!("applications/{APP_ID}.desktop"))
            .display()
    );
    if autostart {
        println!(
            "BackDown's background service now starts when you log in ({})",
            autostart_path().display()
        );
    }
    Ok(ExitCode::SUCCESS)
}

fn remove(p: &Path, removed: &mut Vec<String>) {
    let gone = if p.is_dir() {
        std::fs::remove_dir_all(p).is_ok()
    } else {
        std::fs::remove_file(p).is_ok()
    };
    if gone {
        removed.push(p.display().to_string());
    }
}

fn uninstall() -> Result<ExitCode> {
    let mut removed = Vec::new();
    if cfg!(target_os = "macos") {
        let app = mac_app();
        // only the bundle `desktop install` made (its launcher is a script), never a copied-in app
        if std::fs::read(app.join("Contents/MacOS/BackDown"))
            .is_ok_and(|b| b.starts_with(b"#!/bin/sh"))
        {
            remove(&app, &mut removed);
        }
    } else if cfg!(windows) {
        remove(&windows_menu().join("BackDown.lnk"), &mut removed);
        remove(&windows_menu().join(r"Startup\BackDown.lnk"), &mut removed);
        remove(&locations::state_dir().join("backdown.ico"), &mut removed);
    } else {
        for (p, _, _) in linux_files(Path::new("backdown"), true) {
            remove(&p, &mut removed);
        }
        let mut c = Command::new("update-desktop-database");
        c.arg(xdg("XDG_DATA_HOME", ".local/share").join("applications"));
        quietly(&mut c);
        let _ = c.status();
    }
    if removed.is_empty() {
        println!("nothing to remove: BackDown wasn't added to your applications");
    } else {
        for r in &removed {
            println!("removed {r}");
        }
        println!(
            "Your index, settings and licence stay in {}.",
            locations::state_dir().display()
        );
        if !cfg!(target_os = "linux") {
            println!(
                "BackDown's background service, if you installed it, stays: `backdown service uninstall` removes it."
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_size(b: &[u8]) -> (u32, u32) {
        assert_eq!(&b[..8], b"\x89PNG\r\n\x1a\n");
        (
            u32::from_be_bytes(b[16..20].try_into().unwrap()),
            u32::from_be_bytes(b[20..24].try_into().unwrap()),
        )
    }

    #[test]
    fn icons_are_the_sizes_they_say() {
        for (n, png) in ICONS.iter().chain(MAC_ICONS) {
            assert_eq!(png_size(png), (*n, *n));
        }
        assert!(SVG_ICON.starts_with("<svg") && SVG_ICON.contains("fill-rule=\"evenodd\""));
    }

    #[test]
    fn exec_quoting_follows_the_spec() {
        assert_eq!(exec_arg("/usr/bin/backdown"), "/usr/bin/backdown");
        assert_eq!(exec_arg("/home/a b/backdown"), "\"/home/a b/backdown\"");
        assert_eq!(exec_arg("/opt/$x"), "\"/opt/\\\\$x\"");
        assert_eq!(exec_arg("/opt/100%"), "/opt/100%%");
        let e = desktop_entry("/home/a b/bin/backdown");
        assert!(e.contains("Exec=\"/home/a b/bin/backdown\" app\n"), "{e}");
        assert!(e.contains("TryExec=/home/a b/bin/backdown\n"));
        assert!(e.contains(&format!("Icon={APP_ID}\n")));
        assert!(e.contains("StartupWMClass=BackDown\n"));
        let a = autostart_entry("/usr/bin/backdown");
        assert!(
            a.contains("Exec=/usr/bin/backdown app --no-window\n") && a.contains("NoDisplay=true")
        );
        // in an AppImage, the name alone
        assert!(!desktop_entry("backdown").contains("TryExec"));
    }

    #[test]
    fn info_plist_is_well_formed() {
        let p = info_plist("0.5.0-alpha.2", "backdown");
        assert!(p.contains("<key>CFBundleExecutable</key>\n\t<string>backdown</string>"));
        assert!(p.contains("<key>CFBundleShortVersionString</key>\n\t<string>0.5.0</string>"));
        assert!(p.contains("<string>0.5.0-alpha.2</string>"));
        assert!(p.contains(APP_ID));
        // every key has one value, and the dict closes
        let keys = p.matches("<key>").count();
        let values = p.matches("<string>").count() + p.matches("<true/>").count();
        assert_eq!(keys, values);
        assert!(p.trim_end().ends_with("</plist>"));
        assert_eq!(numeric_version("1.2.3+x"), "1.2.3");
        assert_eq!(numeric_version("junk"), "0.0.0");
    }

    #[test]
    fn launcher_quotes_the_path() {
        let s = mac_launcher(Path::new("/Users/x/it's here/backdown"));
        assert!(s.starts_with("#!/bin/sh\n"));
        assert!(
            s.contains("exec '/Users/x/it'\\''s here/backdown' app \"$@\""),
            "{s}"
        );
    }

    #[test]
    fn icns_holds_every_size() {
        let b = icns();
        assert_eq!(&b[..4], b"icns");
        assert_eq!(
            u32::from_be_bytes(b[4..8].try_into().unwrap()) as usize,
            b.len()
        );
        let mut at = 8;
        let mut seen = Vec::new();
        while at < b.len() {
            let kind = std::str::from_utf8(&b[at..at + 4]).unwrap().to_string();
            let len = u32::from_be_bytes(b[at + 4..at + 8].try_into().unwrap()) as usize;
            let (w, _) = png_size(&b[at + 8..at + len]);
            seen.push((kind, w));
            at += len;
        }
        assert_eq!(at, b.len());
        let want = [
            ("icp4", 16),
            ("icp5", 32),
            ("icp6", 64),
            ("ic07", 128),
            ("ic08", 256),
            ("ic09", 512),
            ("ic10", 1024),
            ("ic11", 32),
            ("ic12", 64),
            ("ic13", 256),
            ("ic14", 512),
        ];
        assert_eq!(
            seen,
            want.iter()
                .map(|(k, n)| (k.to_string(), *n))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn ico_directory_points_at_its_pngs() {
        let b = ico();
        assert_eq!(&b[..4], &[0, 0, 1, 0]);
        let n = u16::from_le_bytes([b[4], b[5]]) as usize;
        assert_eq!(n, 6);
        for i in 0..n {
            let e = &b[6 + 16 * i..6 + 16 * (i + 1)];
            let size = u32::from_le_bytes(e[8..12].try_into().unwrap()) as usize;
            let off = u32::from_le_bytes(e[12..16].try_into().unwrap()) as usize;
            let (w, h) = png_size(&b[off..off + size]);
            let dim = if e[0] == 0 { 256 } else { e[0] as u32 };
            assert_eq!((w, h), (dim, dim));
        }
    }

    #[test]
    fn windows_scripts_are_ascii_and_quote_paths() {
        for s in [
            windows_install_ps1("0.5.0-alpha.1"),
            windows_uninstall_ps1(),
            windows_install_cmd(),
            windows_uninstall_cmd(),
        ] {
            assert!(s.is_ascii(), "Windows PowerShell reads these as ANSI:\n{s}");
        }
        let ps = windows_install_ps1("0.5.0-alpha.1");
        assert!(ps.contains("$version = '0.5.0-alpha.1'"));
        assert!(ps.contains("& $exe @a"));
        assert!(ps.contains(r"'Programs\BackDown'"));
        let lnk = lnk_script(
            Path::new(r"C:\Users\O'Brien\Start Menu\BackDown.lnk"),
            Path::new(r"C:\Users\O'Brien\Programs\BackDown\backdown.exe"),
            "app",
            Path::new(r"C:\x\backdown.ico"),
        );
        assert!(
            lnk.contains(r"CreateShortcut('C:\Users\O''Brien\Start Menu\BackDown.lnk')"),
            "{lnk}"
        );
        assert!(
            lnk.contains(r#"$s.Arguments = '--headless "C:\Users\O''Brien\Programs\BackDown\backdown.exe" app'"#),
            "{lnk}"
        );
        assert!(lnk.contains(r"System32\conhost.exe'") && lnk.contains("$s.WindowStyle = 7"));
        assert!(crlf("a\nb\r\nc").windows(2).all(|w| w != b"\r\r"));
    }

    #[test]
    fn msi_versions_grow_with_releases() {
        let order = [
            "0.2.0-alpha.4",
            "0.2.0-alpha.10",
            "0.2.0-beta.1",
            "0.2.0-rc.1",
            "0.2.0",
            "0.2.1-alpha.1",
            "0.2.1",
            "0.3.0",
        ];
        let key = |v: &str| {
            let m = msi_version(v).unwrap();
            let p: Vec<u32> = m.split('.').map(|x| x.parse().unwrap()).collect();
            (p[0], p[1], p[2])
        };
        for w in order.windows(2) {
            assert!(key(w[0]) < key(w[1]), "{} < {}", w[0], w[1]);
        }
        assert_eq!(msi_version("0.2.0-alpha.4").unwrap(), "0.2.104");
        assert_eq!(msi_version("1.0.0").unwrap(), "1.0.900");
        assert!(msi_version("0.2.66").is_err());
    }

    /// Every start tag closed, nothing an XML parser refuses (like "--" in a comment).
    fn well_formed(xml: &str) {
        let mut r = quick_xml::Reader::from_str(xml);
        let mut depth = 0i32;
        loop {
            match r.read_event() {
                Ok(quick_xml::events::Event::Start(_)) => depth += 1,
                Ok(quick_xml::events::Event::End(_)) => depth -= 1,
                Ok(quick_xml::events::Event::Comment(c)) => {
                    assert!(
                        !String::from_utf8_lossy(&c).contains("--"),
                        "-- in a comment"
                    )
                }
                Ok(quick_xml::events::Event::Eof) => break,
                Ok(_) => {}
                Err(e) => panic!("{e}\n{xml}"),
            }
        }
        assert_eq!(depth, 0, "{xml}");
    }

    #[test]
    fn generated_xml_is_well_formed() {
        well_formed(&wxs("0.2.0-alpha.4").unwrap());
        well_formed(&info_plist("0.2.0-alpha.4", "backdown"));
        well_formed(&metainfo("0.2.0-alpha.4"));
    }

    #[test]
    fn wxs_is_per_user_and_stable() {
        let w = wxs("0.5.0-beta.2").unwrap();
        assert!(w.contains(r#"InstallScope="perUser""#));
        assert!(w.contains(&format!(r#"UpgradeCode="{UPGRADE_CODE}""#)));
        assert!(w.contains(r#"Version="0.5.202""#));
        assert!(w.contains(r#"Arguments="--headless &quot;[INSTALLDIR]backdown.exe&quot; app""#));
        assert!(w.contains("LocalAppDataFolder"));
        assert!(!w.contains("ProgramFilesFolder"));
        assert_eq!(
            w.matches("<Component ").count(),
            w.matches("</Component>").count()
        );
    }

    #[test]
    fn package_file_sets() {
        let linux = package_files("linux", None, "0.5.0").unwrap();
        let names: Vec<&str> = linux.iter().map(|f| f.path.as_str()).collect();
        assert!(names.contains(&"usr/share/applications/io.backdown.BackDown.desktop"));
        assert!(names.contains(&"usr/lib/systemd/user/backdown.service"));
        assert!(names.contains(&"usr/share/icons/hicolor/256x256/apps/io.backdown.BackDown.png"));
        assert!(names.contains(&"usr/share/icons/hicolor/scalable/apps/io.backdown.BackDown.svg"));
        let unit = &linux
            .iter()
            .find(|f| f.path.ends_with("backdown.service"))
            .unwrap()
            .data;
        assert!(String::from_utf8_lossy(unit).contains("ExecStart=/usr/bin/backdown daemon"));
        let app = package_files("appimage", None, "0.5.0").unwrap();
        let run = app.iter().find(|f| f.path == "AppRun").unwrap();
        assert_eq!(run.mode, 0o755);
        assert!(String::from_utf8_lossy(&run.data).contains("set -- app"));
        let mac = package_files("macos", None, "0.5.0").unwrap();
        assert!(
            mac.iter()
                .any(|f| f.path == "BackDown.app/Contents/Resources/BackDown.icns")
        );
        let win = package_files("windows", None, "0.5.0").unwrap();
        let cmd = win
            .iter()
            .find(|f| f.path == "Install BackDown.cmd")
            .unwrap();
        assert!(cmd.data.windows(2).any(|w| w == b"\r\n"));
        assert!(package_files("beos", None, "0.5.0").is_err());
    }
}
