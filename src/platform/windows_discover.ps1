$ErrorActionPreference='Stop'
[Console]::OutputEncoding=[System.Text.UTF8Encoding]::new($false)
$entries=[System.Collections.Generic.List[object]]::new()
$warnings=[System.Collections.Generic.List[string]]::new()
foreach($scope in @('HKCU:','HKLM:')) {
  foreach($suffix in @('Software\Microsoft\Windows\CurrentVersion\Run','Software\Microsoft\Windows\CurrentVersion\RunOnce','Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Run')) {
    $key="$scope\$suffix"
    if(!(Test-Path -LiteralPath $key)){continue}
    try {
      $item=Get-Item -LiteralPath $key
      $branch=if($suffix -like '*WOW6432Node*'){'Run32'}else{'Run'}
      $approved="$scope\Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\$branch"
      foreach($name in $item.GetValueNames()) {
        $value=[string]$item.GetValue($name)
        $approvedKey=Get-Item -LiteralPath $approved -ErrorAction SilentlyContinue
        $bytes=if($approvedKey){$approvedKey.GetValue($name,$null)}else{$null}
        $enabled=!(($bytes -is [byte[]]) -and ($bytes.Length -gt 0) -and ($bytes[0] -in @(3,7)))
        # RunOnce is not controlled through StartupApproved by Windows.
        $kind=if($suffix -like '*RunOnce'){'runonce'}else{'run'}
        if($kind -eq 'runonce') { $warnings.Add("RunOnce entry (inspect-only): $key / $name") }
        $entries.Add(@{id="registry:$key/$name";name=$name;kind=$kind;key=$key;value=$name;approved=$approved;command=$value;location=$key;enabled=$enabled;detail="Registry startup · $scope"})
      }
    } catch { $warnings.Add("Registry $key : $($_.Exception.Message)") }
  }
}
foreach($folder in @([Environment]::GetFolderPath('Startup'),[Environment]::GetFolderPath('CommonStartup'))) {
  if(!$folder -or !(Test-Path -LiteralPath $folder)){continue}
  try {
    $shell=New-Object -ComObject WScript.Shell
    foreach($f in Get-ChildItem -LiteralPath $folder -File -Force) {
      if($f.Name -eq 'desktop.ini'){continue}
      $command=$f.FullName
      if($f.Extension -eq '.lnk') { try { $s=$shell.CreateShortcut($f.FullName);$command='"'+$s.TargetPath+'" '+$s.Arguments } catch {} }
      $disabled=$f.Name.EndsWith('.aservice-disabled')
      $approved='HKCU:\Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\StartupFolder'
      $approvedKey=Get-Item -LiteralPath $approved -ErrorAction SilentlyContinue
      $bytes=if($approvedKey){$approvedKey.GetValue($f.Name,$null)}else{$null}
      $nativeDisabled=($bytes -is [byte[]]) -and ($bytes.Length -gt 0) -and ($bytes[0] -in @(3,7))
      $entries.Add(@{id="startup:$($f.FullName)";name=$f.Name;kind='startup';path=$f.FullName;command=$command;location=$folder;enabled=(!$disabled -and !$nativeDisabled);detail=if($nativeDisabled){'Disabled in Windows Startup Apps; enable there first'}else{'Startup folder item'}})
    }
  } catch { $warnings.Add("Startup folder $folder : $($_.Exception.Message)") }
}
try {
  foreach($t in Get-ScheduledTask -ErrorAction Stop) {
    $actions=@($t.Actions | ForEach-Object { '"'+$_.Execute+'" '+$_.Arguments }) -join ' ; '
    $triggers=@($t.Triggers | ForEach-Object {$_.CimClass.CimClassName}) -join ', '
    $entries.Add(@{id="task:$($t.TaskPath)$($t.TaskName)";name=$t.TaskName;kind='task';path="$($t.TaskPath)$($t.TaskName)";command=$actions;location=$t.TaskPath;enabled=[bool]$t.Settings.Enabled;running=([string]$t.State -eq 'Running');detail="Triggers: $triggers · State: $($t.State)"})
  }
} catch { $warnings.Add("Scheduled Tasks: $($_.Exception.Message)") }
try {
  foreach($s in Get-CimInstance Win32_Service -ErrorAction Stop) {
    $entries.Add(@{id="service:$($s.Name)";name=$s.DisplayName;kind='service';value=$s.Name;command=$s.PathName;location=$s.Name;enabled=($s.StartMode -eq 'Auto');running=($s.State -eq 'Running');pid=[int]$s.ProcessId;detail="Start mode: $($s.StartMode) · State: $($s.State) · Account: $($s.StartName) · Delayed: $($s.DelayedAutoStart)"})
  }
} catch { $warnings.Add("Services: $($_.Exception.Message)") }
@{entries=@($entries.ToArray());warnings=@($warnings.ToArray())} | ConvertTo-Json -Depth 8 -Compress
