"""Exercise service rendering with deployment paths and systemd argument escaping."""
import subprocess
import tempfile
from pathlib import Path

root = Path(__file__).resolve().parents[3]
with tempfile.TemporaryDirectory() as directory:
    output = Path(directory) / 'venera-debug-host.service'
    command = ['python3', str(root / 'scripts/venera-debug-host-service'),
               '--binary', '/usr/local/bin/venera-debug-host',
               '--socket', '/run/user/1000/venera/debug.sock',
               '--control', '/run/user/1000/venera/control.sock',
               '--database', '/home/user/logs $data%/history.db',
               '--max-bytes', '131072', '--output', str(output)]
    subprocess.run(command, check=True, capture_output=True)
    text = output.read_text()
    assert 'ExecStart="/usr/local/bin/venera-debug-host" "receive" "--socket" "/run/user/1000/venera/debug.sock"' in text
    assert '"/home/user/logs $$data%%/history.db"' in text
    assert 'Restart=on-failure\nRestartSec=2s' in text
    assert 'WantedBy=default.target' in text
    invalid = command.copy()
    invalid[invalid.index('--binary') + 1] = 'relative/bin'
    result = subprocess.run(invalid, capture_output=True, text=True)
    assert result.returncode == 2
    assert 'deployment paths must be absolute' in result.stderr
print('deployment rendering: absolute paths, escaping, restart policy checked')
