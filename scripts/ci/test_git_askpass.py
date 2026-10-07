"""Check credential routing with a synthetic token; never use the shell's credential."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("git-askpass.sh")

class CredentialRoutingTest(unittest.TestCase):
    def invoke(self, prompt, token="synthetic-test-token"):
        env = dict(os.environ, CARGO_GITHUB_TOKEN=token)
        return subprocess.run(["bash", str(SCRIPT), prompt], env=env, capture_output=True, text=True)

    def test_dependency_username_and_password(self):
        for suffix in ("", ".git"):
            with self.subTest(suffix=suffix):
                base = "github.com/jarida-io/llama-cpp-rs-giap" + suffix
                user = self.invoke("Username for 'https://" + base + "': ")
                self.assertEqual((user.returncode, user.stdout), (0, "x-access-token\n"))
                password = self.invoke("Password for 'https://x-access-token@" + base + "': ")
                self.assertEqual((password.returncode, password.stdout), (0, "synthetic-test-token\n"))

    def test_unrelated_requests_get_no_credential(self):
        for url in ("github.com", "github.com/another/repo", "github.com.evil.test/jarida-io/llama-cpp-rs-giap", "github.com/jarida-io/llama-cpp-rs-giap-evil", "github.com/jarida-io/llama-cpp-rs-giap/extra"):
            with self.subTest(url=url):
                result = self.invoke("Password for 'https://x-access-token@" + url + "': ")
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(result.stdout, "")

    def test_real_git_credential_prompts_are_scoped(self):
        # No network request and no credential helper/keychain access. Git itself
        # builds the prompt so this catches incorrect assumptions about its URL.
        env = dict(os.environ, CARGO_GITHUB_TOKEN="synthetic-test-token",
                   GIT_ASKPASS=str(SCRIPT.resolve()), GIT_TERMINAL_PROMPT="0",
                   GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull,
                   LC_ALL="C")
        with tempfile.TemporaryDirectory() as directory:
            for path, allowed in (("jarida-io/llama-cpp-rs-giap", True),
                                  ("jarida-io/llama-cpp-rs-giap.git", True),
                                  ("another/repo", False),
                                  ("jarida-io/llama-cpp-rs-giap-evil", False)):
                with self.subTest(path=path):
                    result = subprocess.run(
                        ["git", "-c", "credential.helper=", "-c",
                         "credential.useHttpPath=true", "credential", "fill"],
                        input=f"protocol=https\nhost=github.com\npath={path}\n\n",
                        cwd=directory, env=env, capture_output=True, text=True,
                        timeout=5,
                    )
                    if allowed:
                        self.assertEqual(result.returncode, 0)
                        self.assertIn("password=synthetic-test-token\n", result.stdout)
                    else:
                        self.assertNotEqual(result.returncode, 0)
                        self.assertNotIn("synthetic-test-token", result.stdout)

    def test_missing_credential_fails_closed(self):
        result = self.invoke("Password for 'https://x-access-token@github.com/jarida-io/llama-cpp-rs-giap': ", token="")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")

if __name__ == "__main__":
    unittest.main()
