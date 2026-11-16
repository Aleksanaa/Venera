"""Exercise service rendering with deployment paths and systemd argument escaping."""
import subprocess
import tempfile
from pathlib import Path

root = Path(__file__).resolve().parents[3]
with tempfile.TemporaryDirectory() as directory:
    output = Path(directory) / 'venera-debug-host.service'
    command = ['python3', str(root / 'scripts/venera-debug-host-service'),
               '--binary', '/usr/local/bin/venera-debug-host',
               '--cid', '42',
               '--control', '/run/user/1000/venera/control.sock',
               '--database', '/home/user/logs $data%/history.db',
               '--max-bytes', '131072', '--assets', '/opt/venera/web',
               '--listen', '127.0.0.1:8788', '--output', str(output)]
    subprocess.run(command, check=True, capture_output=True)
    text = output.read_text()
    assert 'ExecStart="/usr/local/bin/venera-debug-host" "serve" "--cid" "42" "--port" "49152"' in text
    assert '"--assets" "/opt/venera/web" "--listen" "127.0.0.1:8788"' in text
    assert '"/home/user/logs $$data%%/history.db"' in text
    assert 'Restart=on-failure\nRestartSec=2s' in text
    assert 'WantedBy=default.target' in text
    subprocess.run(command + ['--no-web'], check=True, capture_output=True)
    assert '"--no-web"' in output.read_text()
    invalid = command.copy()
    invalid[invalid.index('--binary') + 1] = 'relative/bin'
    result = subprocess.run(invalid, capture_output=True, text=True)
    assert result.returncode == 2
    assert 'deployment paths must be absolute' in result.stderr
    subprocess.run(command + ['--port', '50000'], check=True, capture_output=True)
    assert '"--cid" "42" "--port" "50000"' in output.read_text()
    for flag, value in [('--cid', '2'), ('--cid', '4294967295'), ('--port', '0'), ('--port', '4294967295')]:
        result = subprocess.run(command + [flag, value], capture_output=True, text=True)
        assert result.returncode == 2
        assert flag + ' must be between' in result.stderr
print('deployment rendering: absolute paths, escaping, restart policy checked')
