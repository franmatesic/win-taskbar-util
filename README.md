# Win Taskbar Util

`wintaskbarutil` is a small Windows command-line utility for hiding and restoring the native Windows taskbar.

It works by starting a background daemon process that keeps the Explorer taskbar suppressed.

## Commands

```shell
wintaskbarutil show
wintaskbarutil hide
wintaskbarutil status
wintaskbarutil version
wintaskbarutil help
wintaskbarutil autostart enable
wintaskbarutil autostart disable
wintaskbarutil autostart status
```

## Installation

```shell
winget install franmatesic.wintaskbarutil
```