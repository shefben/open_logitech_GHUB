G HUB (ghub-rs)
===============

A standalone peripheral manager for Logitech G devices, speaking HID++ directly. No G HUB, no
Logitech Gaming Software and no Artemis needed.

Two programs, one application
-----------------------------

    ghub-agent.exe   1.6 MB   stays running: devices, lighting, profile rules, macros,
                              remapped keys, the automation API and the G icon in the
                              notification area. No window, no GUI toolkit linked in.
    ghub.exe         9.2 MB   the window. Started when you want it, closed when you don't.

They are installed side by side and find each other that way. While the window is open it borrows
the devices from the agent; when you close it, it exits for real and the agent takes them back.

That is why closing the window is worth doing. Measured on the machine this was built on, the
window costs about 390 MB of working set once its GL context, its swapchain and the two device
renders (3600x1374 and 1440x2858 pixels) are loaded. The agent costs about 7 MB, and 1.8 MB of
that is private. Before the split, closing the window only hid it and the application kept all
390 MB for as long as it was left running.

Running it
----------

ghub.exe runs as it stands, from anywhere, as long as the assets directory and ghub-agent.exe stay
beside it - it starts the agent itself if one is not already running. The icons, the device renders
and the notification-area icon are loaded from the assets directory at run time.

Installing it
-------------

    powershell -NoProfile -ExecutionPolicy Bypass -File install.ps1

Per-user, never elevated:

    files              %LOCALAPPDATA%\Programs\ghub-rs
    Start menu         this account's Start menu
    Installed apps     listed for this user only
    profiles/macros    %APPDATA%\ghub-rs

Nothing is installed as a service, a scheduled task or a driver, and nothing is written outside
this account. "Start with Windows" is a setting inside the application's Options page, so the
choice stays with you rather than being made by the installer.

Uninstalling
------------

    powershell -NoProfile -ExecutionPolicy Bypass -File uninstall.ps1

Profiles and macros are kept. Add -RemoveSettings to delete those too.

What it does with your hardware
-------------------------------

  * It reads device identity, firmware version, battery, DPI, report rate and the lighting
    capabilities each device reports, and writes only what you change.
  * It does not update or flash firmware. The firmware version is read and displayed; there is no
    code in this application that writes firmware.
  * It does not update itself and has no plugin mechanism.
  * Lighting is written to RAM, not to the device's onboard configuration, unless you explicitly
    use "Save to device" with the switch in Options turned on. That path previews what it will
    write, keeps a copy of what it replaces, and reads back afterwards.
  * Closing the window closes it. The agent keeps running, so remapped keys, macros, profile
    rules and lighting keep working - right-click the G icon in the notification area to switch
    profile, or to quit everything. "Quit G HUB completely" on the Options page does the same.
  * "Start with Windows" registers the agent, not the window: nothing appears on screen at
    logon, and what runs is the 1.6 MB half.

Other software that controls the same devices
---------------------------------------------

If G HUB, Logitech Gaming Software or OpenRGB is running, both programs write to the same
devices and settings may appear to revert a second after being applied. The Diagnostics page
names anything it finds. This application never closes them for you.

If something does not work
--------------------------

Open Diagnostics, switch on protocol logging, reproduce the problem, then press "Save
diagnostics". The file holds device ids, capabilities and the log. It does not hold keystrokes.

Diagnostics also reports the hand-over: if it says no background agent is running, the window is
managing the devices on its own and closing it will stop doing so. Reinstalling puts
ghub-agent.exe back beside it.

One known Windows limit, stated there as well: input to a window running as administrator is
neither visible to nor injectable by a program that is not, so macros and remapped keys do
nothing in such a window. This application does not elevate to work around it.
