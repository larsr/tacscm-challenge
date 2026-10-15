"""Every published house agent id builds its kernel; entry points match pyproject."""

from __future__ import annotations

from pathlib import Path
import tomllib
import unittest

from tac_scm_gallery import house

AGENTS_DIRECTORY = Path(__file__).resolve().parents[1]


class HouseAgentTests(unittest.TestCase):
    def test_console_scripts_cover_every_id(self) -> None:
        project = tomllib.loads((AGENTS_DIRECTORY / "pyproject.toml").read_text())
        scripts = project["project"]["scripts"]
        self.assertEqual(set(scripts), set(house.AGENTS))
        for agent_id, target in scripts.items():
            module, _, attribute = target.partition(":")
            self.assertEqual(module, "tac_scm_gallery.house")
            self.assertTrue(callable(getattr(house, attribute)), agent_id)

    def test_every_id_builds_its_kernel(self) -> None:
        arguments = house.Arguments("test", "0" * 64, "test", 0)
        for agent_id, build in house.AGENTS.items():
            with self.subTest(agent_id=agent_id):
                try:
                    kernel = build(arguments)
                except house.ModelsMissing as error:
                    self.skipTest(str(error))
                self.assertTrue(callable(kernel.decide))


if __name__ == "__main__":
    unittest.main()
