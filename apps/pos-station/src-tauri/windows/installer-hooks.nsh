; Copyright (c) 2026 Pizza 4P's. All rights reserved.
; Proprietary and confidential. Internal use only. See LICENSE.
;
; NSIS hooks for the per-machine installer: start POS Station at every login, for every user of the
; store PC, and stop doing so on uninstall. NOT YET RUN ON WINDOWS - see docs/guides/pos-station.md.

!macro NSIS_HOOK_POSTINSTALL
  SetRegView 64
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Run" "POS Station" '"$INSTDIR\pos-station.exe"'
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  SetRegView 64
  DeleteRegValue HKLM "Software\Microsoft\Windows\CurrentVersion\Run" "POS Station"
!macroend
