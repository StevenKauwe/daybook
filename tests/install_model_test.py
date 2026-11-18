import hashlib
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from scripts.install_model import install_model


class ModelInstallTests(unittest.TestCase):
    model = b"fixture model"
    spec = {
        "source": "https://example.test/pinned/model.gguf",
        "sha256": hashlib.sha256(model).hexdigest(),
        "bytes": len(model),
    }

    def download(self, payload):
        def write_output(args, check):
            Path(args[args.index("--output") + 1]).write_bytes(payload)
        return patch("scripts.install_model.subprocess.run", side_effect=write_output)

    def test_fresh_install_then_offline_reuse(self):
        with tempfile.TemporaryDirectory() as tmp, self.download(self.model) as curl:
            models = Path(tmp) / "data/models"
            install_model(models, self.spec)
            install_model(models, self.spec)
            self.assertEqual((models / "model.gguf").read_bytes(), self.model)
            curl.assert_called_once()

    def test_rejected_download_preserves_cache_until_verified_retry(self):
        with tempfile.TemporaryDirectory() as tmp:
            models = Path(tmp)
            target = models / "model.gguf"
            target.write_bytes(b"previous cache")
            for payload in (b"short", b"x" * len(self.model)):
                with self.subTest(payload=payload), self.download(payload), self.assertRaises(SystemExit):
                    install_model(models, self.spec)
                self.assertEqual(target.read_bytes(), b"previous cache")
                self.assertEqual(list(models.iterdir()), [target])
            with self.download(self.model):
                install_model(models, self.spec)
            self.assertEqual(target.read_bytes(), self.model)
