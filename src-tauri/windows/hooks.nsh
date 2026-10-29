; Krak was called Kryo Shard. Installing Krak removes the old app silently; its settings and
; library stay where they are and Krak picks them up on first launch.

!macro NSIS_HOOK_PREINSTALL
  ReadRegStr $R9 HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Kryo Shard" "UninstallString"
  ${If} $R9 != ""
    DetailPrint "Removing Kryo Shard (now Krak)..."
    ExecWait '$R9 /S'
  ${EndIf}
  ; Its "launch at startup" entry, if any (Krak writes its own).
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Kryo Shard"
!macroend
