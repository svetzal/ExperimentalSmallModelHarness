"""Classify native Harness endpoints independently of outer process success."""
POLICY_EVENTS = {
    'agent.turn.hidden_only_no_action_hard_failed': 'hidden_only_no_action_stop',
    'agent.turn.empty_response_hard_failed': 'empty_response_stop',
    'agent.validation.repair_hard_failed': 'repair_stop',
    'agent.validation.repair_depth_hard_failed': 'repair_depth_stop',
    'agent.action_boundary.hard_failed': 'action_boundary_stop',
}

def classify_terminal(events, return_code, *, max_iterations, max_model_interactions):
    terminals=[e for e in events if e['kind'] in ('run.finished','run.failed')]
    if not terminals:
        raise ValueError('Missing Harness terminal event')
    terminal=terminals[-1]
    if terminal['kind']=='run.failed':
        p=terminal['payload']
        if return_code==1 and p.get('phase')=='agent.stream' and p.get('error')==f'Maximum model interactions exceeded: limit {max_model_interactions}':
            return 'interaction_limit'
        raise ValueError('Unexpected Harness error: '+str(p))
    if return_code!=0:
        raise ValueError('Harness terminal/process status mismatch')
    for event in reversed(events):
        if event['kind'] in POLICY_EVENTS:
            return POLICY_EVENTS[event['kind']]
    lines=terminal['payload'].get('final_summary','').splitlines()
    if any(line.strip()=='DONE' for line in lines):
        return 'done'
    if any(line.strip().split(maxsplit=1)[:1]==['FAIL'] for line in lines):
        return 'fail'
    if max((e['payload'].get('turn',0) for e in events if e['kind']=='agent.turn.started'),default=0)==max_iterations:
        return 'outer_turn_limit'
    raise ValueError('Unrecognized Harness terminal')
