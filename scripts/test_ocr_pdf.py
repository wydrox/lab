import importlib.util
from pathlib import Path
import sys
import types
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("ocr_pdf", Path(__file__).with_name("ocr_pdf.py"))
ocr = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ocr)


class OcrTests(unittest.TestCase):
    def test_template_each_page_and_result_text(self):
        calls = []
        processor = object()
        model = types.SimpleNamespace(config={"model_type": "glm_ocr"})
        def generate(*args, **kwargs):
            calls.append(kwargs)
            return types.SimpleNamespace(text="tekst " + kwargs["image"], generation_tokens=10)
        fake = types.ModuleType("mlx_vlm")
        fake.load = lambda path: (model, processor)
        fake.generate = generate
        prompt = types.ModuleType("mlx_vlm.prompt_utils")
        def template(p, config, text, **kwargs):
            self.assertIs(p, processor)
            self.assertEqual(kwargs["num_images"], 1)
            return "szablon obrazu"
        prompt.apply_chat_template = template
        with patch.dict(sys.modules, {"mlx_vlm": fake, "mlx_vlm.prompt_utils": prompt}):
            result = ocr.recognize(["a.png", "b.png"], "local-model", 100)
            self.assertEqual(result, ["tekst a.png", "tekst b.png"])
            self.assertEqual([c["prompt"] for c in calls], ["szablon obrazu"] * 2)
            with self.assertRaises(ValueError):
                ocr.recognize(["a.png"], "local-model", 10)

    def test_poppler_and_env_are_restricted(self):
        self.assertEqual(ocr.MAX_PDF_BYTES, 40_000_000)
        env = ocr.tool_env()
        self.assertEqual(env["HF_HUB_OFFLINE"], "1")
        self.assertEqual(env["PYTHONNOUSERSITE"], "1")
        self.assertNotIn("PYTHONPATH", env)
        with patch.object(ocr.os, "access", return_value=True), patch.object(ocr.Path, "is_file", return_value=True):
            self.assertEqual(ocr.poppler_bin("pdfinfo"), Path("/opt/homebrew/bin/pdfinfo"))


if __name__ == "__main__":
    unittest.main()
