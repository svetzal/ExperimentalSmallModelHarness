import unittest
from terminal_outcome import classify_terminal

class TerminalOutcomeTests(unittest.TestCase):
    def classify(self,events,code=0):
        return classify_terminal(events,code,max_iterations=4,max_model_interactions=24)

    def test_native_policy_stop_despite_zero_exit(self):
        events=[{'kind':'agent.turn.started','payload':{'turn':3}},
                {'kind':'agent.turn.hidden_only_no_action_hard_failed','payload':{'turn':3}},
                {'kind':'run.finished','payload':{'final_summary':'turn 3 produced 2 consecutive hidden-only no-action responses'}}]
        self.assertEqual(self.classify(events),'hidden_only_no_action_stop')

    def test_interaction_limit_is_exact_provider_error_is_rejected(self):
        event={'kind':'run.failed','payload':{'phase':'agent.stream','error':'Maximum model interactions exceeded: limit 24'}}
        self.assertEqual(self.classify([event],1),'interaction_limit')
        event['payload']['error']='Unexpected HTTP disconnect'
        with self.assertRaises(ValueError):self.classify([event],1)

    def test_tokens_and_outer_budget(self):
        end={'kind':'run.finished','payload':{'final_summary':'DONE'}}
        self.assertEqual(self.classify([end]),'done')
        end['payload']['final_summary']='failing'
        with self.assertRaises(ValueError):self.classify([end])
        self.assertEqual(self.classify([{'kind':'agent.turn.started','payload':{'turn':4}},end]),'outer_turn_limit')
        end['payload']['final_summary']='FAIL blocked'
        self.assertEqual(self.classify([end]),'fail')
