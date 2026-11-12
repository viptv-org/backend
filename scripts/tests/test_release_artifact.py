import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('release_artifact', Path(__file__).parents[1] / 'release-artifact.py')
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


class ReleaseAbiTests(unittest.TestCase):
    def test_accepts_bookworm_compatible_runtime_libraries(self):
        dynamic = '\n'.join('Shared library: [' + name + ']' for name in ['libgcc_s.so.1', 'libm.so.6', 'libc.so.6'])
        result = release.inspect_abi(dynamic, 'Name: GLIBC_2.34\nName: GLIBC_2.35\nName: GLIBC_2.2.5')
        self.assertEqual(result['maximum_glibc_required'], '2.35')
        self.assertEqual(len(result['needed_libraries']), 3)

    def test_rejects_newer_glibc_than_the_build_baseline(self):
        with self.assertRaisesRegex(ValueError, 'exceeds'):
            release.inspect_abi('Shared library: [libc.so.6]', 'Name: GLIBC_2.36')

    def test_rejects_unbundled_shared_libraries(self):
        with self.assertRaisesRegex(ValueError, 'libraries'):
            release.inspect_abi('Shared library: [libssl.so.3]', 'Name: GLIBC_2.35')

    def test_rejects_private_or_missing_glibc_requirements(self):
        for value in ['Name: GLIBC_PRIVATE', 'Name: GLIBC_ABI_DT_RELR', '']:
            with self.subTest(value=value), self.assertRaises(ValueError):
                release.inspect_abi('Shared library: [libc.so.6]', value)


class BuildEvidenceTests(unittest.TestCase):
    def test_accepts_one_successful_cargo_completion(self):
        release.require_completed_build([{'reason': 'compiler-artifact'}, {'reason': 'build-finished', 'success': True}])

    def test_rejects_missing_failed_or_ambiguous_completion(self):
        success = {'reason': 'build-finished', 'success': True}
        for records in [[], [{'reason': 'compiler-artifact'}], [{'reason': 'build-finished', 'success': False}], [success, success]]:
            with self.subTest(records=records), self.assertRaisesRegex(ValueError, 'completion_required'):
                release.require_completed_build(records)


if __name__ == '__main__':
    unittest.main()
