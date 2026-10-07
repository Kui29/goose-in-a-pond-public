"""Regression tests for the assertions used by the live server harness."""
import contextlib
import io
import unittest
from unittest.mock import patch

import live_checks


class PlayerRefusalTests(unittest.TestCase):
    def host_predicate(self):
        predicates = []

        def capture(label, code, want, body, *checks):
            if label == "an offline pond refuses the player window's request to Apple":
                predicates.extend(predicate for name, predicate in checks if name == "names the host")
            return True

        with patch.object(live_checks, "call", return_value=(200, {})), \
                patch.object(live_checks, "expect", side_effect=capture), \
                contextlib.redirect_stdout(io.StringIO()):
            live_checks.section_music_player()
        self.assertEqual(len(predicates), 1)
        return predicates[0]

    def test_accepts_the_refused_destination(self):
        self.assertTrue(self.host_predicate()({
            "allowed": False,
            "reason": 'network_mode = "offline" refused an outbound request to js-cdn.music.apple.com: network access is disabled',
        }))

    def test_rejects_decoys_and_malformed_responses(self):
        predicate = self.host_predicate()
        for body in [
            {"reason": "refused an outbound request to js-cdn.music.apple.com.evil.test: denied"},
            {"reason": "refused an outbound request to eviljs-cdn.music.apple.com: denied"},
            {"url": "https://js-cdn.music.apple.com/", "reason": "denied"},
            {"reason": "https://evil.test/js-cdn.music.apple.com"},
            {"reason": None}, {}, None,
        ]:
            with self.subTest(body=body):
                self.assertFalse(predicate(body))


if __name__ == "__main__":
    unittest.main()
