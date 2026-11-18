"""Install and probe a disposable networkless Windows Server Eval VM."""
from hashlib import sha256
import json
from pathlib import Path
from secrets import token_urlsafe
import shutil
import socket
import subprocess
import sys
import time
import xml.etree.ElementTree as ET

HERE = Path(__file__).resolve().parent
MARKER = "WINDOWS_EVALUATION_RESULT "
COMPONENT = (
    'processorArchitecture="amd64" publicKeyToken="31bf3856ad364e35" '
    'language="neutral" versionScope="nonSxS"'
)


def answer_file():
    password = token_urlsafe(21) + "9wX!"
    letters = " ".join(letter for letter in "DEFGHIJKLMNOPQRSTUVWXYZ" if letter != "X")
    return rf"""<?xml version="1.0" encoding="utf-8"?>
<unattend xmlns="urn:schemas-microsoft-com:unattend" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State">
  <settings pass="windowsPE">
    <component name="Microsoft-Windows-International-Core-WinPE" {COMPONENT}>
      <SetupUILanguage><UILanguage>en-US</UILanguage></SetupUILanguage>
      <InputLocale>en-US</InputLocale><SystemLocale>en-US</SystemLocale>
      <UILanguage>en-US</UILanguage><UserLocale>en-US</UserLocale>
    </component>
    <component name="Microsoft-Windows-Setup" {COMPONENT}>
      <DiskConfiguration>
        <Disk wcm:action="add">
          <DiskID>0</DiskID><WillWipeDisk>false</WillWipeDisk>
          <CreatePartitions>
            <CreatePartition wcm:action="add"><Order>1</Order><Type>EFI</Type><Size>260</Size></CreatePartition>
            <CreatePartition wcm:action="add"><Order>2</Order><Type>MSR</Type><Size>16</Size></CreatePartition>
            <CreatePartition wcm:action="add"><Order>3</Order><Type>Primary</Type><Extend>true</Extend></CreatePartition>
          </CreatePartitions>
          <ModifyPartitions>
            <ModifyPartition wcm:action="add"><Order>1</Order><PartitionID>1</PartitionID><Format>FAT32</Format><Label>System</Label></ModifyPartition>
            <ModifyPartition wcm:action="add"><Order>2</Order><PartitionID>3</PartitionID><Format>NTFS</Format><Label>Windows</Label><Letter>C</Letter></ModifyPartition>
          </ModifyPartitions>
        </Disk>
        <WillShowUI>OnError</WillShowUI>
      </DiskConfiguration>
      <ImageInstall><OSImage>
        <InstallFrom><MetaData wcm:action="add"><Key>/IMAGE/INDEX</Key><Value>1</Value></MetaData></InstallFrom>
        <InstallTo><DiskID>0</DiskID><PartitionID>3</PartitionID></InstallTo>
        <WillShowUI>OnError</WillShowUI>
      </OSImage></ImageInstall>
      <UserData><AcceptEula>true</AcceptEula><FullName>Evaluation Probe</FullName><Organization>Disposable Evaluation Guest</Organization></UserData>
    </component>
  </settings>
  <settings pass="specialize">
    <component name="Microsoft-Windows-Shell-Setup" {COMPONENT}>
      <ComputerName>EVAL-PROBE</ComputerName><TimeZone>UTC</TimeZone>
    </component>
    <component name="Microsoft-Windows-Deployment" {COMPONENT}>
      <RunSynchronous><RunSynchronousCommand wcm:action="add">
        <Order>1</Order>
        <Path>cmd.exe /d /c for %D in ({letters}) do @if exist %D:\EVAL-MEDIA.TAG (md C:\Windows\Setup\Scripts 2&gt;nul &amp; copy /Y %D:\SetupComplete.cmd C:\Windows\Setup\Scripts\SetupComplete.cmd &amp; copy /Y %D:\guest-check.ps1 C:\Windows\Setup\Scripts\guest-check.ps1)</Path>
      </RunSynchronousCommand></RunSynchronous>
    </component>
  </settings>
  <settings pass="oobeSystem">
    <component name="Microsoft-Windows-Shell-Setup" {COMPONENT}>
      <OOBE><HideEULAPage>true</HideEULAPage><HideWirelessSetupInOOBE>true</HideWirelessSetupInOOBE><HideOnlineAccountScreens>true</HideOnlineAccountScreens><ProtectYourPC>3</ProtectYourPC></OOBE>
      <UserAccounts><AdministratorPassword><Value>{password}</Value><PlainText>true</PlainText></AdministratorPassword></UserAccounts>
    </component>
  </settings>
</unattend>
""".encode()


def build_media(scratch):
    staged = scratch / "answer"
    staged.mkdir(mode=0o700)
    xml_bytes = answer_file()
    ET.fromstring(xml_bytes)
    (staged / "Autounattend.xml").write_bytes(xml_bytes)
    (staged / "EVAL-MEDIA.TAG").write_text("Disposable Windows evaluation probe\n", encoding="ascii")
    (staged / "SetupComplete.cmd").write_bytes(
        b"@echo off\r\n"
        b'"%SystemRoot%\\System32\\WindowsPowerShell\\v1.0\\powershell.exe" '
        b'-NoProfile -NonInteractive -ExecutionPolicy RemoteSigned '
        b'-File "%SystemRoot%\\Setup\\Scripts\\guest-check.ps1"\r\n'
        b"exit /b %ERRORLEVEL%\r\n"
    )
    shutil.copyfile(HERE / "guest-check.ps1", staged / "guest-check.ps1")
    output = scratch / "answer.iso"
    subprocess.run(
        ["xorriso", "-as", "mkisofs", "-quiet", "-J", "-r", "-V", "EVAL_PROBE",
         "-o", str(output), str(staged)], check=True,
    )
    return output


def qmp(path, command, arguments=None):
    with socket.socket(socket.AF_UNIX) as client:
        client.settimeout(3)
        client.connect(str(path))
        with client.makefile("rwb") as stream:
            greeting = json.loads(stream.readline())
            if "QMP" not in greeting:
                raise RuntimeError("Bad QEMU monitor greeting")
            for action in ({"execute": "qmp_capabilities"}, {"execute": command, "arguments": arguments or {}}):
                stream.write(json.dumps(action).encode() + b"\r\n")
                stream.flush()
                while True:
                    message = json.loads(stream.readline())
                    if "error" in message:
                        raise RuntimeError(message["error"])
                    if "return" in message:
                        break
            return message["return"]


def checked_result(serial):
    if not serial.exists():
        return None
    for line in serial.read_text(encoding="ascii", errors="replace").splitlines():
        if not line.startswith(MARKER):
            continue
        try:
            result = json.loads(line.removeprefix(MARKER))
        except json.JSONDecodeError:
            continue
        print(MARKER + json.dumps(result, sort_keys=True), flush=True)
        expected_digest = sha256(b"synthetic disconnected Windows evaluation").hexdigest()
        if (
            result.get("status") != "passed"
            or result.get("phase") != "SetupComplete"
            or not result.get("edition", "").startswith("ServerStandardEval")
            or result.get("physical_network_adapters") != 0
            or result.get("default_routes") != 0
            or result.get("sample_sha256") != expected_digest
        ):
            raise RuntimeError("The evaluation guest did not pass its installed-system checks")
        return result
    return None


def show_boot_diagnostic(scratch, qmp_path):
    screen = scratch / "screen.ppm"
    try:
        qmp(qmp_path, "screendump", {"filename": str(screen)})
        output = subprocess.run(
            ["tesseract", str(screen), "stdout", "--psm", "11"],
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            check=True, text=True, timeout=10,
        ).stdout
        normalized = " ".join(output.lower().split())
        markers = (
            "uefi interactive shell", "press any key", "installing windows server",
            "windows setup", "setup failed", "windows cannot", "getting ready",
            "administrator", "login", "restart", "something went wrong",
        )
        print("Guest screen markers:", [item for item in markers if item in normalized], flush=True)
    except (OSError, RuntimeError, ValueError, subprocess.SubprocessError) as error:
        print("Guest boot diagnostic unavailable:", type(error).__name__, flush=True)


def run(scratch):
    answer = build_media(scratch)
    disk = scratch / "fresh-system.qcow2"
    vars_path = scratch / "OVMF_VARS.fd"
    qmp_path = scratch / "qemu.sock"
    serial = scratch / "serial.log"
    console = scratch / "qemu.log"
    shutil.copyfile("/usr/share/OVMF/OVMF_VARS_4M.fd", vars_path)
    subprocess.run(["qemu-img", "create", "-f", "qcow2", str(disk), "72G"], check=True)
    cmd = [
        "qemu-system-x86_64", "-machine", "q35", "-accel", "kvm", "-cpu", "host",
        "-smp", "4", "-m", "6144", "-nic", "none", "-no-user-config", "-nodefaults",
        "-display", "none", "-monitor", "none", "-device", "VGA",
        "-qmp", f"unix:{qmp_path},server=on,wait=off", "-serial", f"file:{serial}",
        "-drive", "file=/usr/share/OVMF/OVMF_CODE_4M.fd,if=pflash,format=raw,unit=0,readonly=on",
        "-drive", f"file={vars_path},if=pflash,format=raw,unit=1",
        "-drive", f"file={disk},if=none,id=system,format=qcow2",
        "-device", "ide-hd,bus=ide.0,drive=system,bootindex=2",
        "-drive", f"file={scratch / 'evaluation.iso'},if=none,id=installer,format=raw,media=cdrom,readonly=on",
        "-device", "ide-cd,bus=ide.1,drive=installer,bootindex=1",
        "-drive", f"file={answer},if=none,id=answer,format=raw,media=cdrom,readonly=on",
        "-device", "ide-cd,bus=ide.2,drive=answer",
    ]
    print("Starting q35 UEFI guest with KVM, an empty sparse 72 GiB disk, and -nic none", flush=True)
    with console.open("wb") as output:
        guest = subprocess.Popen(cmd, stdin=subprocess.DEVNULL, stdout=output, stderr=output)
        start = time.monotonic()
        key_times = iter(range(1, 16))
        next_key = next(key_times, None)
        next_update = 30
        try:
            while (elapsed := time.monotonic() - start) < 1500:
                if checked_result(serial):
                    return
                if guest.poll() is not None:
                    raise RuntimeError(f"QEMU exited before the guest result ({guest.returncode})")
                if next_key is not None and elapsed >= next_key:
                    if qmp_path.exists():
                        qmp(qmp_path, "human-monitor-command", {"command-line": "sendkey spc"})
                    next_key = next(key_times, None)
                if elapsed >= next_update:
                    allocated = disk.stat().st_blocks * 512
                    print(f"Guest wait {int(elapsed)}s. Disk allocated {allocated} bytes.", flush=True)
                    if elapsed >= 240 and allocated < 32 * 1024 * 1024:
                        raise RuntimeError("Windows Setup did not start writing to the guest disk")
                    next_update += 30
                time.sleep(1)
            raise TimeoutError("Windows Setup did not emit an installed guest result in 25 minutes")
        except Exception:
            show_boot_diagnostic(scratch, qmp_path)
            print(console.read_text(errors="replace")[-4000:], flush=True)
            if serial.exists():
                print(serial.read_text(errors="replace")[-2000:], flush=True)
            raise
        finally:
            if guest.poll() is None:
                guest.terminate()
                try:
                    guest.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    guest.kill()
                    guest.wait()


if __name__ == "__main__":
    run(Path(sys.argv[1]))
