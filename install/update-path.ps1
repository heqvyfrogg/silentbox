# インストーラ／アンインストーラから呼ぶ PATH 更新スクリプト。
#
# なぜ NSIS の ReadRegStr / WriteRegExpandStr を使わないか（実測）:
#   NSIS の ReadRegStr は 1023 文字までしか読めない。1024 文字以上は例外も
#   警告もなく空文字列になる（実測: 1023 -> len=1023, 1024 -> len=0,
#   1100 -> len=0）。/DNSIS_MAX_STRLEN を 8192 にしても変わらなかった。
#   長い PATH を持つ環境（まさに壊れる条件）で ReadRegStr を使うと、既存値を
#   空と誤認して PATH 全体を $INSTDIR だけで上書きしてしまう。
#
# なぜ setx を使わないか（実測）:
#   setx は保存値を 1024 文字で打ち切る。終了コードは 0、警告は stderr のみ。
#
# したがって .NET のレジストリ API を直接使う。長さ制限は無く、値の種類
# （REG_SZ / REG_EXPAND_SZ）も保てる。
#
# 引数は環境変数で渡す（コマンドラインの引用符問題を避ける）。
#   TBX_PATH_ACTION = add | remove
#   TBX_INSTALL_DIR = 追加／削除するディレクトリ（絶対パス）
#
# 標準出力の 1 行目: ADDED / REMOVED / ALREADY / NOTFOUND / EMPTY
# 失敗時は終了コード 1。

$ErrorActionPreference = "Stop"

$action = $env:TBX_PATH_ACTION
$dir = $env:TBX_INSTALL_DIR

if ([string]::IsNullOrEmpty($action) -or [string]::IsNullOrEmpty($dir)) {
    [Console]::Error.WriteLine("TBX_PATH_ACTION と TBX_INSTALL_DIR が必要です")
    exit 1
}

try {
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey("Environment", $true)
    if ($null -eq $key) {
        [Console]::Error.WriteLine("HKCU\Environment を開けません")
        exit 1
    }

    try {
        # PATH は REG_SZ と REG_EXPAND_SZ のどちらもあり得る。展開せずに読む
        # （展開して書き戻すと %VAR% が実パスに化けて元の意味が失われる）。
        $current = [string]$key.GetValue(
            "Path", "",
            [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)

        try { $kind = $key.GetValueKind("Path") } catch { $kind = [Microsoft.Win32.RegistryValueKind]::String }
        $newKind = if ($kind -eq [Microsoft.Win32.RegistryValueKind]::ExpandString) {
            [Microsoft.Win32.RegistryValueKind]::ExpandString
        } else {
            [Microsoft.Win32.RegistryValueKind]::String
        }

        $entries = $current -split ';'

        if ($action -eq "add") {
            foreach ($e in $entries) {
                # 大文字小文字を無視して完全一致で比較する（前方一致では
                # C:\...\silentbox2 のような別ディレクトリを誤認する）。
                if ([string]::Equals($e.Trim(), $dir, [StringComparison]::OrdinalIgnoreCase)) {
                    Write-Output "ALREADY"
                    exit 0
                }
            }
            $appended = if ($current.Length -eq 0) { $dir } else { $current.TrimEnd(';') + ';' + $dir }
            $key.SetValue("Path", $appended, $newKind)
            Write-Output "ADDED"
            exit 0
        }

        if ($action -eq "remove") {
            $kept = @()
            $removed = $false
            foreach ($e in $entries) {
                if ([string]::Equals($e.Trim(), $dir, [StringComparison]::OrdinalIgnoreCase)) {
                    $removed = $true
                    continue
                }
                if ($e.Length -eq 0) { continue }
                $kept += $e
            }
            if (-not $removed) {
                Write-Output "NOTFOUND"
                exit 0
            }
            $updated = ($kept -join ';')
            $key.SetValue("Path", $updated, $newKind)
            if ($updated.Length -eq 0) { Write-Output "EMPTY" } else { Write-Output "REMOVED" }
            exit 0
        }

        [Console]::Error.WriteLine("不明な action: $action")
        exit 1
    }
    finally {
        $key.Close()
    }
}
catch {
    [Console]::Error.WriteLine("PATH を更新できません: $($_.Exception.Message)")
    exit 1
}