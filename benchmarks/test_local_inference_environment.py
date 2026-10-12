import copy
import unittest
from pathlib import Path
from local_inference_environment import SERVICE, add_bridge, routed_environment, validate_ports


class LocalInferenceTests(unittest.TestCase):
    def test_preserves_payload_settings_and_routes_both_providers(self):
        env = {'OLLAMA_HOST': 'http://host.docker.internal:11436',
               'OMLX_HOST': 'http://host.docker.internal:8000/v1',
               'HTTP_PROXY': 'http://filtered:8080', 'NO_PROXY': '*', 'TEMPERATURE': '1.0'}
        result = routed_environment(env, (11436, 8000))
        self.assertEqual(result['OLLAMA_HOST'], f'http://{SERVICE}:11436')
        self.assertEqual(result['OMLX_HOST'], f'http://{SERVICE}:8000/v1')
        self.assertEqual(result['HTTP_PROXY'], env['HTTP_PROXY'])
        self.assertEqual(result['TEMPERATURE'], '1.0')
        self.assertNotIn('*', result['NO_PROXY'])
        self.assertEqual(env['NO_PROXY'], '*')

    def test_rejects_unregistered_remote_and_credential_endpoints(self):
        for endpoint in ('http://example.com:11436', 'http://host.docker.internal:80',
                         'http://user:password@host.docker.internal:11436',
                         'https://host.docker.internal:11436',
                         'http://host.docker.internal:11436/?token=secret'):
            with self.subTest(endpoint=endpoint), self.assertRaises(ValueError):
                routed_environment({'OLLAMA_HOST': endpoint}, (11436,))
        with self.assertRaises(ValueError):
            routed_environment({}, (11436,))

    def test_invalid_ports(self):
        for ports in ((), (0,), (65536,), (11436, 11436), (True,)):
            with self.subTest(ports=ports), self.assertRaises(ValueError):
                validate_ports(ports)

    def test_topology_has_no_worker_egress_or_published_ports(self):
        compose = {'services': {'main': {'networks': ['pier-egress-internal']}},
                   'networks': {'pier-egress-internal': {'internal': True}}}
        result = add_bridge(copy.deepcopy(compose), Path('/bridge.py'), 'sha256:'+'a'*64, (11436,))
        self.assertEqual(result['services']['main']['networks'], ['pier-egress-internal'])
        bridge = result['services'][SERVICE]
        self.assertNotIn('ports', bridge)
        self.assertEqual(bridge['command'], ['11436'])
        self.assertEqual(bridge['cap_drop'], ['ALL'])
        with self.assertRaises(ValueError):
            add_bridge(compose, Path('/bridge.py'), 'ubuntu:latest', (11436,))
        compose['services']['main']['networks'].append('default')
        with self.assertRaises(ValueError):
            add_bridge(compose, Path('/bridge.py'), 'sha256:'+'a'*64, (11436,))


if __name__ == '__main__':
    unittest.main()
