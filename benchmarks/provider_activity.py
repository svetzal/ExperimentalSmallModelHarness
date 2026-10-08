"""Read-only interpretation of the verified oMLX 0.7 /api/status counters.

active_requests counts EngineCore output collectors, including waiting requests.
waiting_requests counts Scheduler.waiting, not an additional disjoint population.
The counters are sampled from different structures, so their union is unknown;
max(active, waiting) is a conservative lower bound, never their sum.
"""


def omlx_activity(status):
    """Classify overlap for a trial permitted to own at most one request.

    This does not establish client identity. Admission still requires an idle
    host; multiple models and cross-provider activity remain separate checks.
    Invalid telemetry raises rather than becoming a fabricated ownership claim.
    """
    active = status['active_requests']
    waiting = status['waiting_requests']
    for name, value in [('active_requests', active), ('waiting_requests', waiting)]:
        if type(value) is not int or value < 0:
            raise ValueError(f'{name} must be a nonnegative integer')
    minimum_requests = max(active, waiting)
    return {
        'active_collectors': active,
        'waiting_requests': waiting,
        'minimum_distinct_requests': minimum_requests,
        'idle': active == 0 and waiting == 0,
        'overlap': minimum_requests > 1,
        'semantics': 'waiting_included_in_active_collectors',
    }
