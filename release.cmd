@echo off
rem Builds the shareable zip from any folder; options are passed on, e.g. "release -AllVersions".
cd /d "%~dp0app" && npm run release -- %*
