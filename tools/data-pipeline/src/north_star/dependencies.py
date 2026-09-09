"""Bounded closure of an explicitly supplied ancestry graph.

This proves closure over declared edges only, not that a producer declared all
real causal parents. Independent source-to-context lineage auditing still applies.
"""
from collections.abc import Mapping


def parent_closure(root, graph, *, max_nodes=100000, max_edges=1000000):
    if not isinstance(root, str) or not root.strip() or not isinstance(graph, Mapping):
        raise ValueError('root and graph must be exact identifiers and a mapping')
    if any(type(n) is not int or n <= 0 for n in (max_nodes, max_edges)):
        raise ValueError('invalid traversal budget')
    state = {}
    stack = [(root, False)]
    edges = 0
    while stack:
        node, leaving = stack.pop()
        if leaving:
            state[node] = 2
            continue
        if state.get(node) == 1:
            raise ValueError('cyclic dependency')
        if state.get(node) == 2:
            continue
        if node not in graph:
            raise ValueError('missing dependency node')
        if len(state)+1 > max_nodes:raise ValueError('dependency budget exceeded')
        parents=graph[node]
        if not isinstance(parents,(list,tuple)):raise ValueError('invalid parent list')
        if edges+len(parents) > max_edges:raise ValueError('dependency budget exceeded')
        if any(not isinstance(p,str) or not p.strip() for p in parents):
            raise ValueError('invalid dependency identifiers')
        if len(set(parents)) != len(parents):
            raise ValueError('duplicate dependency identifiers')
        state[node] = 1
        edges += len(parents)
        if len(state) > max_nodes or edges > max_edges:
            raise ValueError('dependency traversal budget exceeded')
        stack.append((node, True))
        stack.extend((p, False) for p in reversed(parents))
    return tuple(sorted(set(state) - {root}))
