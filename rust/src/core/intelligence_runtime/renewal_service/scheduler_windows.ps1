# SPDX-License-Identifier: Apache-2.0
# Static Windows PowerShell 5.1 COM adapter. Request values are read only
# from LEANCTX_TASK_REQUEST; no value is interpolated into this script.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
Set-StrictMode -Version Latest

$RegistrationMarker = 'LeanCTX verified renewal task v1'
# GetTask reports a missing on-disk task as HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND).
$TaskNotFoundHResult = -2147024894
$SystemSid = 'S-1-5-18'

function Test-TaskNotFound($record) {
    $exception = $record.Exception
    while ($null -ne $exception) {
        if ([int]$exception.HResult -eq $TaskNotFoundHResult) {
            return $true
        }
        $exception = $exception.InnerException
    }
    return $false
}

function Get-Task($folder, [string]$label) {
    try {
        return $folder.GetTask($label)
    }
    catch {
        if (Test-TaskNotFound $_) {
            return $null
        }
        throw
    }
}

function Write-Result($value) {
    $json = ConvertTo-Json -InputObject $value -Compress -Depth 4
    [Console]::Out.WriteLine($json)
}

function Assert-Label([string]$label) {
    if ([string]::IsNullOrEmpty($label) -or $label.Length -gt 128 -or
        $label -eq '.' -or $label -eq '..' -or
        $label -cnotmatch '\A[A-Za-z0-9_.-]+\z') {
        throw 'invalid label'
    }
}

function Assert-TaskRequest($request) {
    $task = $request.task
    if ($null -eq $task -or
        $task.label -isnot [string] -or
        $task.sid -isnot [string] -or
        $task.host -isnot [string] -or
        $task.arguments -isnot [string] -or
        $task.working_directory -isnot [string] -or
        $task.start_boundary -isnot [string] -or
        $task.label -cne $request.label) {
        throw 'invalid task'
    }
    Assert-Label ([string]$task.label)
    $currentSid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    if ([string]$task.sid -cne $currentSid -or
        [string]$task.sid -cnotmatch '\AS-1-[0-9]+(?:-[0-9]+)+\z') {
        throw 'invalid principal'
    }
    if ([string]::IsNullOrEmpty([string]$task.host) -or
        ([string]$task.host).Length -gt 32767 -or
        [string]$task.host -notmatch '\A(?:[A-Za-z]:[\\/]|\\\\[^\\]+\\[^\\]+)' -or
        ([string]$task.host).Contains([char]0) -or
        ([string]$task.host).Contains('%') -or
        [string]::IsNullOrEmpty([string]$task.working_directory) -or
        ([string]$task.working_directory).Length -gt 32767 -or
        [string]$task.working_directory -notmatch '\A(?:[A-Za-z]:[\\/]|\\\\[^\\]+\\[^\\]+)' -or
        ([string]$task.working_directory).Contains([char]0) -or
        ([string]$task.working_directory).Contains('%') -or
        ([string]$task.arguments).Length -gt 32767 -or
        ([string]$task.arguments).Contains([char]0) -or
        ([string]$task.arguments).Contains('%') -or
        [string]::IsNullOrEmpty([string]$task.start_boundary) -or
        ([string]$task.start_boundary).Length -gt 128 -or
        [string]$task.start_boundary -notmatch '\A[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(?:\.[0-9]{1,7})?(?:Z|[+-][0-9]{2}:[0-9]{2})?\z') {
        throw 'invalid task values'
    }
}

function Get-PrincipalSid([string]$userId) {
    if ($userId -cmatch '\AS-1-[0-9]+(?:-[0-9]+)+\z') {
        return $userId
    }
    $account = [System.Security.Principal.NTAccount]::new($userId)
    return $account.Translate([System.Security.Principal.SecurityIdentifier]).Value
}

function Assert-TaskSecurity($registered, $task) {
    $sddl = [string]$registered.GetSecurityDescriptor(5)
    $descriptor = [System.Security.AccessControl.RawSecurityDescriptor]::new($sddl)
    if ($null -eq $descriptor.Owner -or
        $descriptor.Owner.Value -cne [string]$task.sid -or
        ($descriptor.ControlFlags -band [System.Security.AccessControl.ControlFlags]::DiscretionaryAclProtected) -eq 0) {
        throw 'invalid task owner or DACL'
    }

    $acl = $descriptor.DiscretionaryAcl
    if ($null -eq $acl -or $acl.Count -lt 2) {
        throw 'invalid task DACL'
    }
    $allowedSids = @([string]$task.sid, $SystemSid)
    $seenSids = @()
    foreach ($ace in $acl) {
        if ($ace -isnot [System.Security.AccessControl.CommonAce] -or
            $ace.AceQualifier -ne [System.Security.AccessControl.AceQualifier]::AccessAllowed -or
            $ace.IsInherited -or
            $ace.AceFlags -ne [System.Security.AccessControl.AceFlags]::None -or
            $ace.AccessMask -ne 2032127) {
            throw 'invalid task DACL entry'
        }
        $sid = $ace.SecurityIdentifier.Value
        if ($sid -cnotin $allowedSids) {
            throw 'unexpected task DACL principal'
        }
        if ($sid -cnotin $seenSids) {
            $seenSids += $sid
        }
    }
    if ($seenSids.Count -ne 2) {
        throw 'incomplete task DACL'
    }
}

function Assert-TaskIdentity($registered, $task, [bool]$allowDisabled) {
    if ([string]$registered.Name -cne [string]$task.label) {
        throw 'task name mismatch'
    }
    $definition = $registered.Definition
    if ([string]$definition.RegistrationInfo.Description -cne $RegistrationMarker) {
        throw 'task marker mismatch'
    }

    $principal = $definition.Principal
    if ((Get-PrincipalSid ([string]$principal.UserId)) -cne [string]$task.sid -or
        [int]$principal.LogonType -ne 3 -or
        [int]$principal.RunLevel -ne 0) {
        throw 'task principal mismatch'
    }

    if ([bool]$registered.Enabled -ne $true -and
        (-not $allowDisabled -or [bool]$registered.Enabled -ne $false)) {
        throw 'task enabled state mismatch'
    }
    $settings = $definition.Settings
    if ([int]$settings.MultipleInstances -ne 2 -or
        [bool]$settings.StartWhenAvailable -ne $false -or
        [bool]$settings.AllowHardTerminate -ne $false -or
        [bool]$settings.DisallowStartIfOnBatteries -ne $false -or
        [bool]$settings.StopIfGoingOnBatteries -ne $false -or
        [bool]$settings.RunOnlyIfIdle -ne $false -or
        [bool]$settings.RunOnlyIfNetworkAvailable -ne $false -or
        [int]$settings.RestartCount -ne 0 -or
        [bool]$settings.WakeToRun -ne $false -or
        [string]$settings.ExecutionTimeLimit -cne 'PT0S') {
        throw 'task settings mismatch'
    }

    $triggers = $definition.Triggers
    if ([int]$triggers.Count -ne 1) {
        throw 'task trigger count mismatch'
    }
    $trigger = $triggers.Item(1)
    if ([int]$trigger.Type -ne 1 -or
        [bool]$trigger.Enabled -ne $true -or
        [string]$trigger.StartBoundary -cne [string]$task.start_boundary -or
        -not [string]::IsNullOrEmpty([string]$trigger.EndBoundary) -or
        [string]$trigger.RandomDelay -cnotin @('', 'PT0S') -or
        [string]$trigger.Repetition.Interval -cne 'PT1M' -or
        -not [string]::IsNullOrEmpty([string]$trigger.Repetition.Duration) -or
        [bool]$trigger.Repetition.StopAtDurationEnd -ne $false) {
        throw 'task trigger mismatch'
    }

    $actions = $definition.Actions
    if ([int]$actions.Count -ne 1) {
        throw 'task action count mismatch'
    }
    $action = $actions.Item(1)
    if ([int]$action.Type -ne 0 -or
        [string]$action.Path -cne [string]$task.host -or
        [string]$action.Arguments -cne [string]$task.arguments -or
        [string]$action.WorkingDirectory -cne [string]$task.working_directory) {
        throw 'task action mismatch'
    }

    Assert-TaskSecurity $registered $task
}

function Get-TaskStatus($registered) {
    $state = [int]$registered.State
    $stateName = switch ($state) {
        1 { 'disabled' }
        2 { 'queued' }
        3 { 'ready' }
        4 { 'running' }
        default { 'unknown' }
    }
    $instances = $registered.GetInstances(0)
    return @{
        registered = $true
        enabled = [bool]$registered.Enabled
        state = [string]$stateName
        running_instances = [int]$instances.Count
    }
}

try {
    $rawRequest = [Environment]::GetEnvironmentVariable('LEANCTX_TASK_REQUEST')
    if ([string]::IsNullOrEmpty($rawRequest) -or $rawRequest.Length -gt 30000) {
        throw 'invalid request'
    }
    $request = ConvertFrom-Json -InputObject $rawRequest
    Assert-Label ([string]$request.label)
    if ($request.action -isnot [string] -or $request.label -isnot [string]) {
        throw 'invalid request types'
    }
    if ([string]$request.action -notin @('exists', 'status', 'install', 'remove-disable', 'remove-state', 'remove-delete')) {
        throw 'invalid action'
    }
    if ([string]$request.action -ne 'exists') {
        Assert-TaskRequest $request
    }

    $service = New-Object -ComObject Schedule.Service
    $null = $service.Connect()
    $folder = $service.GetFolder('\')
    $registered = Get-Task $folder ([string]$request.label)

    switch ([string]$request.action) {
        'exists' {
            Write-Result @{ exists = ($null -ne $registered) }
            exit 0
        }
        'status' {
            if ($null -eq $registered) {
                Write-Result @{ registered = $false }
                exit 0
            }
            Assert-TaskIdentity $registered $request.task $true
            Write-Result (Get-TaskStatus $registered)
            exit 0
        }
        'install' {
            if ($null -ne $registered) {
                Assert-TaskIdentity $registered $request.task $true
                if (-not [bool]$registered.Enabled) {
                    $registered.Enabled = $true
                    $registered = Get-Task $folder ([string]$request.label)
                }
                if ($null -eq $registered) { throw 'task disappeared' }
                Assert-TaskIdentity $registered $request.task $false
                Write-Result (Get-TaskStatus $registered)
                exit 0
            }

            $task = $request.task
            $definition = $service.NewTask(0)
            $definition.RegistrationInfo.Description = $RegistrationMarker
            $definition.Principal.UserId = [string]$task.sid
            $definition.Principal.LogonType = 3
            $definition.Principal.RunLevel = 0

            $settings = $definition.Settings
            $settings.Enabled = $true
            $settings.MultipleInstances = 2
            $settings.StartWhenAvailable = $false
            $settings.AllowHardTerminate = $false
            $settings.DisallowStartIfOnBatteries = $false
            $settings.StopIfGoingOnBatteries = $false
            $settings.RunOnlyIfIdle = $false
            $settings.RunOnlyIfNetworkAvailable = $false
            $settings.RestartCount = 0
            $settings.WakeToRun = $false
            $settings.ExecutionTimeLimit = 'PT0S'

            $trigger = $definition.Triggers.Create(1)
            $trigger.Enabled = $true
            $trigger.StartBoundary = [string]$task.start_boundary
            $trigger.RandomDelay = 'PT0S'
            $trigger.Repetition.Interval = 'PT1M'
            # Leave the new trigger's end/duration unset for indefinite repetition.
            $trigger.Repetition.StopAtDurationEnd = $false

            $action = $definition.Actions.Create(0)
            $action.Path = [string]$task.host
            $action.Arguments = [string]$task.arguments
            $action.WorkingDirectory = [string]$task.working_directory

            $sddl = 'O:' + [string]$task.sid + 'D:P(A;;FA;;;' +
                [string]$task.sid + ')(A;;FA;;;' + $SystemSid + ')'
            # TASK_CREATE (2) is create-only; never update or replace a collision.
            $registered = $folder.RegisterTaskDefinition(
                [string]$task.label,
                $definition,
                2,
                [string]$task.sid,
                $null,
                3,
                $sddl
            )
            Assert-TaskIdentity $registered $task $false
            Write-Result (Get-TaskStatus $registered)
            exit 0
        }
        'remove-disable' {
            if ($null -eq $registered) {
                Write-Result @{ registered = $false; disabled = $true }
                exit 0
            }
            Assert-TaskIdentity $registered $request.task $true
            if ([bool]$registered.Enabled) {
                $registered.Enabled = $false
            }
            $registered = Get-Task $folder ([string]$request.label)
            if ($null -eq $registered) {
                Write-Result @{ registered = $false; disabled = $true }
                exit 0
            }
            Assert-TaskIdentity $registered $request.task $true
            if ([bool]$registered.Enabled) {
                throw 'task disable failed'
            }
            Write-Result @{ registered = $true; disabled = $true }
            exit 0
        }
        'remove-state' {
            if ($null -eq $registered) {
                Write-Result @{ registered = $false; drained = $true }
                exit 0
            }
            Assert-TaskIdentity $registered $request.task $true
            if ([bool]$registered.Enabled) {
                throw 'task unexpectedly enabled'
            }
            $status = Get-TaskStatus $registered
            $status.drained = ($status.running_instances -eq 0 -and
                $status.state -in @('disabled', 'ready'))
            Write-Result $status
            exit 0
        }
        'remove-delete' {
            if ($null -eq $registered) {
                Write-Result @{ registered = $false; deleted = $false }
                exit 0
            }
            Assert-TaskIdentity $registered $request.task $true
            if ([bool]$registered.Enabled) {
                throw 'task unexpectedly enabled'
            }
            $status = Get-TaskStatus $registered
            if ($status.running_instances -ne 0 -or
                $status.state -notin @('disabled', 'ready')) {
                Write-Result @{ registered = $true; drained = $false; deleted = $false }
                exit 0
            }

            # This invocation performs a fresh identity/ACL check immediately before deletion.
            $registered = Get-Task $folder ([string]$request.label)
            if ($null -eq $registered) {
                Write-Result @{ registered = $false; deleted = $false }
                exit 0
            }
            Assert-TaskIdentity $registered $request.task $true
            if ([bool]$registered.Enabled) {
                throw 'task unexpectedly enabled'
            }
            $status = Get-TaskStatus $registered
            if ($status.running_instances -ne 0 -or
                $status.state -notin @('disabled', 'ready')) {
                Write-Result @{ registered = $true; drained = $false; deleted = $false }
                exit 0
            }
            try {
                $folder.DeleteTask([string]$request.label, 0)
            }
            catch {
                if (Test-TaskNotFound $_) {
                    Write-Result @{ registered = $false; deleted = $false }
                    exit 0
                }
                throw
            }
            Write-Result @{ registered = $false; deleted = $true }
            exit 0
        }
    }
    throw 'invalid operation'
}
catch {
    # Scheduler diagnostics and filesystem paths never leave this adapter.
    [Console]::Error.WriteLine('task scheduler operation failed')
    exit 1
}
