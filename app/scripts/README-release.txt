Stackyard {{VERSION}} - portable local dev stack for Windows 10/11 (x64)
=========================================================================

Apache, PHP, MySQL, Node.js, Composer, Git and Python in one folder, with a small window (and a tray
icon) to start/stop services, switch versions, manage the PATH and add local sites. Everything,
settings included, stays inside this folder. The window is in English, German, Spanish, French,
Polish, Portuguese or Ukrainian (Settings tab; by default the Windows language).


1. INSTALL
----------
- Before unzipping: right-click the zip > Properties > tick "Unblock" > OK. Otherwise Windows marks
  every extracted file as "downloaded from the internet".
- Unzip to a folder whose path has only English letters, e.g. D:\Stackyard (spaces are fine;
  letters such as a-ogonek, o-acute or z-dot are not: PHP and MySQL cannot handle them).
- Run Stackyard.exe. It is not code-signed, so SmartScreen may say "Windows protected your PC":
  click "More info" > "Run anyway".
- Nothing else to install: the Visual C++ runtime that Apache, PHP and MySQL need is included next
  to them. The window uses Microsoft Edge WebView2, which is part of Windows 11 (and of an up-to-date
  Windows 10 with Edge).


2. FIRST STEPS
--------------
- Services tab > "Start all", then open http://localhost/ (a status page with phpinfo()).
- Put projects in www\. For a project at its own address, add a virtual host in the Virtual hosts
  tab, e.g. "shop" -> www\shop\public, then open http://shop.localhost/.
- MySQL: user root, no password, port 3306. You may set a root password: Stackyard stops MySQL with
  its own account "stackyard_shutdown" (it can only shut the server down). Do not delete that account.
- Tools & PATH tab: switch on PATH for the tools you want in every terminal (php, composer, node,
  git...). Open a new terminal afterwards. "Open terminal" in the Services tab works without that.
- Closing the window keeps Stackyard and its services running in the tray (icon next to the clock).
  Use Quit to exit.
- Settings tab > Startup: start Stackyard when you sign in to Windows, and restore the services that
  were running when it was closed.


3. COMING FROM LARAGON (or XAMPP)
---------------------------------
- Ports: quit Laragon first, or change Stackyard's ports in the Settings tab.
- Projects: copy them into www\, or add a virtual host that points to their current folder.
  Laragon's http://project.test becomes http://project.localhost/ (update APP_URL in .env).
- MySQL data, same 8.4 series: with Laragon's and Stackyard's MySQL stopped, copy the folder
  laragon\data\mysql-8.4 to Stackyard's data\mysql-8.4 (replace it if it already exists). Stackyard
  upgrades it on the next start. Keep the Laragon copy until everything works.
  Other versions (5.7, 8.0): export with mysqldump in Laragon and import in Stackyard instead.
- PATH: Laragon's entries stay in your PATH and may still win. Check with "where php" in a new
  terminal. Remove old entries in Windows: Win+R > sysdm.cpl > Advanced > Environment Variables,
  in BOTH lists ("User variables" and "System variables" > Path).


4. MORE
-------
- New tool versions: PHP, Node.js and Composer from Tools & PATH > "Check for updates". Any other
  version: unzip into bin\<tool>\<version-folder>\ and press Refresh (PHP must be Thread Safe x64).
- Folders: www\ (sites), data\ (databases), config\ (settings and generated configs; put your own
  additions in config\apache|php|mysql\conf.d\), logs\.
- Problems: send logs\stackyard.log plus the log of the failing service (apache_error.log,
  php_error.log or mysql_error.log) and a screenshot of the message.
