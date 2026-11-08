; tbx（silentbox）Windows インストーラ
;
; 設計方針:
;   - 管理者権限を要求しない（ユーザー単位で完結する）。
;   - アンインストール時に、利用者の設定・キャッシュ・資格情報を勝手に消さない。
;     消すかどうかは確認ダイアログで選ばせる。無音時は「残す」。
;   - パスは固定文字列で埋め込まず、$INSTDIR からの相対で扱う。
;   - 任意セクションは既定で「未選択」。利用者が明示的に選んだときだけ実行する
;     （/S の無音インストールでも何も起きない）。
;
; ビルド:
;   cd install
;   makensis /DVERSION=0.2.0 /DEXE=..\installer-input\tbx.exe installer.nsi
;
; `/D` の値は引用符なしの 1 トークンとして解釈されるため、空白を含む絶対
; パスは渡せない（実測）。リポジトリ直下に exe を置き、install から見た
; 相対パスで渡す。
;
; このファイルは UTF-8 BOM 付きで保存すること。BOM が無いと NSIS は ANSI
; （西側コードページ）として読み、日本語が化ける（実測）。

Unicode true
!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "FileFunc.nsh"
; レジストリ変更を他プロセスへ通知する定数（HWND_BROADCAST / WM_SETTINGCHANGE）。
!include "WinMessages.nsh"

!ifndef VERSION
  !define VERSION "0.0.0"
!endif
!ifndef VERSION4
  ; VIProductVersion は X.X.X.X の 4 桁数値しか受け付けない。
  !define VERSION4 "${VERSION}.0"
!endif
!ifndef EXE
  !error "EXE を指定してください（/DEXE=path）"
!endif
!ifndef WINFSP_MSI
  !define WINFSP_MSI "winfsp.msi"
!endif

!define APP_NAME "silentbox"
!define APP_DISPLAY "silentbox (tbx)"
!define APP_PUBLISHER "heqvyfrogg"
!define APP_EXE "tbx.exe"
!define APP_KEY "Software\silentbox"

Name "${APP_DISPLAY} ${VERSION}"
OutFile "..\tbx-${VERSION}-setup.exe"
InstallDir "$LOCALAPPDATA\silentbox"
InstallDirRegKey HKCU "${APP_KEY}" "InstallDir"
RequestExecutionLevel user
SetCompressor /SOLID lzma

VIProductVersion "${VERSION4}"
VIAddVersionKey "ProductName" "${APP_DISPLAY}"
VIAddVersionKey "CompanyName" "${APP_PUBLISHER}"
VIAddVersionKey "FileDescription" "TeraBox の非公式クライアント"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "LegalCopyright" "MIT License (tbx) / WinFsp: Copyright (C) Bill Zissimopoulos (GPLv3)"

!define MUI_ABORTWARNING

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "Japanese"
!insertmacro MUI_LANGUAGE "English"

; ---- 本体 ----
Section "本体（必須）" SEC_CORE
  SectionIn RO
  SetOutPath "$INSTDIR"
  ; NSIS の File はソースのベース名をそのまま使う。入力の exe 名を
  ; tbx.exe に固定するため /oname を付ける（付け忘れると
  ; tbx-installer-input.exe のまま入り、アンインストーラの削除対象とも
  ; 食い違う。実測で確認）。
  File /oname=${APP_EXE} "${EXE}"
  File /oname=README.md "..\README.md"
  File /oname=CHANGELOG.md "..\CHANGELOG.md"
  SetOutPath "$INSTDIR\docs"
  File "..\docs\limitations.md"
  File "..\docs\protocol.md"
  ; WinFsp のライセンス（DLL / MSI を同梱する場合は表示義務がある）。
  File "..\docs\THIRD-PARTY-NOTICES.md"

  SetOutPath "$INSTDIR"

  WriteRegStr HKCU "${APP_KEY}" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "${APP_KEY}" "Version" "${VERSION}"

  ; ユーザーの PATH へ $INSTDIR を追記する。
  ;
  ; NSIS の ReadRegStr は 1023 文字までしか読めず、1024 文字以上は例外も
  ; 警告もなく空文字列になる（実測: 1023 -> len=1023、1024 と 1100 -> len=0。
  ; /DNSIS_MAX_STRLEN=8192 でも変わらない）。長い PATH を持つ環境（まさに
  ; 壊れる条件）で ReadRegStr を使うと、既存値を空と誤認して PATH 全体を
  ; $INSTDIR だけで上書きしてしまう（実測で 2593 文字 -> 37 文字に壊した）。
  ;
  ; setx も使えない。setx は保存値を 1024 文字で打ち切る（終了コード 0、
  ; 警告は stderr のみ）。
  ;
  ; したがって .NET のレジストリ API を使う補助スクリプトへ委譲する。
  ; 長さ制限が無く、値の種類（REG_SZ / REG_EXPAND_SZ）も保てる。
  ; `$PLUGINSDIR` は InitPluginsDir を呼ぶまで作られない。忘れると File は
  ; 通ったように見えるが、実行時に「.ps1 が存在しません」で失敗する（実測）。
  InitPluginsDir
  File "/oname=$PLUGINSDIR\update-path.ps1" "update-path.ps1"

  ; PowerShell へ渡す値は環境変数で渡す（コマンドラインの引用符問題を避ける）。
  ; PATH の値そのものではなくディレクトリだけを渡すので、長さの心配も無い。
  System::Call 'kernel32::SetEnvironmentVariable(t "TBX_PATH_ACTION", t "add") i .r0'
  System::Call 'kernel32::SetEnvironmentVariable(t "TBX_INSTALL_DIR", t "$INSTDIR") i .r0'

  nsExec::ExecToStack '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$PLUGINSDIR\update-path.ps1"'
  Pop $R1   ; 終了コード
  Pop $R0   ; 標準出力の 1 行目（ALREADY / ADDED / ...）
  DetailPrint "PATH 更新: $R0（終了コード $R1）"

  ${If} $R1 != "0"
    ; 黙って続行しない。インストール自体は完了させるが、利用者へ明示する。
    MessageBox MB_ICONEXCLAMATION|MB_OK \
      "PATH を更新できませんでした（$R0）。$\r$\n$\r$\nインストールは完了していますが、コマンドとして使うには手動で次を PATH に追加してください:$\r$\n$INSTDIR"
  ${EndIf}

  ; レジストリを変えただけでは、既に起動しているプロセス（エクスプローラ等）
  ; は新しい PATH を読み直さない。設定変更を通知する。
  ; 補助スクリプトが成功した場合だけ通知する（失敗時に通知しても意味がない）。
  ${If} $R0 == "ADDED"
    SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment" /TIMEOUT=5000
  ${EndIf}

  WriteUninstaller "$INSTDIR\uninstall.exe"

  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\silentbox" \
    "DisplayName" "${APP_DISPLAY}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\silentbox" \
    "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\silentbox" \
    "Publisher" "${APP_PUBLISHER}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\silentbox" \
    "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\silentbox" \
    "NoModify" 1
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\silentbox" \
    "NoRepair" 1
SectionEnd

; ---- WinFsp の導入（任意・既定で未選択） ----
Section /o "WinFsp も入れる（ドライブとして使うなら必要）" SEC_WINFSP
  ; WinFsp のインストーラを**未改変のまま**同梱し、それを起動する。
  ; ライセンス特例は「未改変のインストーラ配布」と「未改変の DLL とのリンク」
  ; だけを許すため、MSI を書き換えたり中身の DLL だけ抜き出したりしない。
  SetOutPath "$INSTDIR\third-party"
  File /oname=winfsp.msi "${WINFSP_MSI}"

  ; MSI 自体の署名を確かめてから起動する（改変・破損を検出する）。
  ; 署名が無い/壊れている場合は実行せず、手動の案内だけ出す。
  ; NSIS の文字列は `$` を変数参照として展開するため、PowerShell を直接
  ; 書くと壊れる（実測: unknown variable "s" の警告が出た）。検証用の
  ; スクリプトを一時ファイルへ書き出し、そのパスだけを渡す。
  DetailPrint "WinFsp のインストーラを確認しています"
  InitPluginsDir
  FileOpen $R3 "$PLUGINSDIR\verify-winfsp.ps1" w
  FileWrite $R3 "$$s = Get-AuthenticodeSignature -LiteralPath $\"$$env:TBX_WINFSP_MSI$\"$\r$\n"
  FileWrite $R3 "if ($$s.Status -eq $\"Valid$\") { exit 0 } else { exit 1 }$\r$\n"
  FileClose $R3

  System::Call "kernel32::SetEnvironmentVariable(t $\"TBX_WINFSP_MSI$\", t $\"$INSTDIR\third-party\winfsp.msi$\")"
  StrCpy $R1 0
  nsExec::ExecToStack 'powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$PLUGINSDIR\verify-winfsp.ps1"'
  Pop $R1
  Pop $R0

  ${If} $R1 == "0"
    DetailPrint "WinFsp を導入します（管理者権限の確認が出ます）"
    ; /qn ではなく /passive を使う。ドライバの導入には昇格が要るため、
    ; 完全な無音では失敗する環境がある。進行バーは出るが操作は不要。
    ExecWait '"$SYSDIR\msiexec.exe" /i "$INSTDIR\third-party\winfsp.msi" /passive /norestart' $R2
    ${If} $R2 != 0
      MessageBox MB_ICONEXCLAMATION|MB_OK \
        "WinFsp の導入が完了しませんでした（msiexec 終了コード $R2）。$\r$\n$\r$\n手動で次を実行してください:$\r$\n$INSTDIR\third-party\winfsp.msi$\r$\n$\r$\nWinFsp が無くても WebDAV モードで使えます。"
    ${EndIf}
  ${Else}
    MessageBox MB_ICONEXCLAMATION|MB_OK \
      "WinFsp のインストーラの署名を確認できませんでした（改変または破損の可能性）。$\r$\n実行は行いません。$\r$\n$\r$\n公式の配布元 https://winfsp.dev/rel/ から導入してください。"
  ${EndIf}
SectionEnd

; ---- 自動起動（任意・既定で未選択） ----
Section /o "PC 起動時に自動でマウント" SEC_AUTOSTART
  WriteRegStr HKCU "${APP_KEY}" "AutoStart" "1"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "silentbox" \
    '"$INSTDIR\${APP_EXE}" mount T:'
SectionEnd

!insertmacro MUI_FUNCTION_DESCRIPTION_BEGIN
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_CORE} \
    "tbx 本体とドキュメントをインストールします。管理者権限は不要です。"
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_WINFSP} \
    "WinFsp のインストーラ（未改変）を同梱して起動します。ドライブとして見せる場合に必要です。管理者権限の確認が出ます。"
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_AUTOSTART} \
    "PC 起動時に T: へ自動でマウントします。ログイン済みである必要があります。"
!insertmacro MUI_FUNCTION_DESCRIPTION_END

Section "Uninstall"
  ; 自動起動の登録を外す。
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "silentbox"

  ; 名前は `APP_EXE` と一致させる（片方だけ直すと消し忘れる）。
  Delete "$INSTDIR\${APP_EXE}"
  Delete "$INSTDIR\README.md"
  Delete "$INSTDIR\CHANGELOG.md"
  Delete "$INSTDIR\docs\limitations.md"
  Delete "$INSTDIR\docs\protocol.md"
  Delete "$INSTDIR\docs\THIRD-PARTY-NOTICES.md"
  Delete "$INSTDIR\third-party\winfsp.msi"
  RMDir "$INSTDIR\third-party"
  RMDir "$INSTDIR\docs"
  Delete "$INSTDIR\uninstall.exe"

  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\silentbox"
  DeleteRegKey HKCU "${APP_KEY}"

  ; PATH から $INSTDIR を外す。外さないと、次に起動したプロセスが存在しない
  ; ディレクトリを PATH に持ち続ける（インストーラで追記した分を対称に戻す）。
  ; 加減算は補助スクリプトへ任せる（NSIS の文字列 API は 1023 文字で切れる）。
  InitPluginsDir
  File "/oname=$PLUGINSDIR\update-path.ps1" "update-path.ps1"
  System::Call 'kernel32::SetEnvironmentVariable(t "TBX_PATH_ACTION", t "remove") i .r0'
  System::Call 'kernel32::SetEnvironmentVariable(t "TBX_INSTALL_DIR", t "$INSTDIR") i .r0'
  nsExec::ExecToStack '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$PLUGINSDIR\update-path.ps1"'
  Pop $R1
  Pop $R0
  ${If} $R0 == "REMOVED"
    SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment" /TIMEOUT=5000
  ${EndIf}

  ; 利用者のデータは勝手に消さない。確認してから消す。
  ; ただし無音（/S）では確認を出さない。出したままだと操作者がいない現場で
  ; 止まり、アンインストーラが完了しない（実測: /S で MessageBox が待ち続けた）。
  ; 無音時の既定は「データを残す」（安全側）。
  IfSilent keep_data
  MessageBox MB_YESNO|MB_ICONQUESTION \
    "設定・キャッシュ・保存済みパスワードも削除しますか？$\r$\n$\r$\n「いいえ」を選ぶと残ります（再インストール時に引き継がれます）。$\r$\n保存済みの暗号化パスワードを消すと、暗号化したファイルは復号できなくなります。" \
    IDNO keep_data
  RMDir /r "$LOCALAPPDATA\tbx"
  RMDir /r "$APPDATA\tbx"
  keep_data:
  ; RMDir は空ディレクトリしか消せず、作業ディレクトリが $INSTDIR のままだと
  ; 自分自身を消せない（実測: 空のディレクトリが残った）。
  ; 利用者のデータは上の確認で消さない選択ができるので、ここでは
  ; インストール物だけを再帰で片付ける。
  SetOutPath $TEMP
  RMDir /r "$INSTDIR"
SectionEnd
