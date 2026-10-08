"""Regressions for a queued owned request versus genuinely multiple requests."""
import unittest
from provider_activity import omlx_activity


class ActivityTests(unittest.TestCase):
    def test_one_owned_request_may_be_active_and_waiting(self):
        result = omlx_activity({'active_requests': 1, 'waiting_requests': 1})
        self.assertFalse(result['overlap'])
        self.assertFalse(result['idle'])
        self.assertEqual(result['minimum_distinct_requests'], 1)

    def test_idle_requires_both_counters_zero(self):
        for active, waiting in [(0, 0), (1, 0), (0, 1), (1, 1)]:
            with self.subTest(active=active, waiting=waiting):
                result = omlx_activity({'active_requests': active, 'waiting_requests': waiting})
                self.assertEqual(result['idle'], active == waiting == 0)
                self.assertFalse(result['overlap'])

    def test_multiple_requests_still_trip_without_adding_counters(self):
        for active, waiting in [(2, 0), (2, 1), (2, 2), (0, 2), (1, 2), (3, 0)]:
            with self.subTest(active=active, waiting=waiting):
                self.assertTrue(omlx_activity({'active_requests': active, 'waiting_requests': waiting})['overlap'])

    def test_bad_measurement_is_not_an_ownership_verdict(self):
        for value in [None, -1, True, 1.0, '1']:
            for field in ['active_requests', 'waiting_requests']:
                status = {'active_requests': 0, 'waiting_requests': 0, field: value}
                with self.subTest(status=status), self.assertRaises(ValueError):
                    omlx_activity(status)
        with self.assertRaises(KeyError):
            omlx_activity({})

    def test_request_lifecycle_never_double_counts_one_request(self):
        timeline = [(0, 0), (1, 0), (1, 1), (1, 0), (0, 0)]
        verdicts = [omlx_activity({'active_requests': a, 'waiting_requests': w}) for a, w in timeline]
        self.assertFalse(any(v['overlap'] for v in verdicts))
        self.assertEqual([v['idle'] for v in verdicts], [True, False, False, False, True])


if __name__ == '__main__':
    unittest.main()
