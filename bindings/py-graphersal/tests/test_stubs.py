"""The type stubs (`graphersal.pyi`) and docstrings cover the module's whole public surface."""

import ast
import inspect
import pathlib

import graphersal

STUB = pathlib.Path(__file__).resolve().parent.parent / "graphersal.pyi"


def stub_tree():
    return ast.parse(STUB.read_text(encoding="utf-8"))


def runtime_public_names():
    # The extension module's own `__all__` (re-exported by the package `__init__`).
    return set(graphersal.__all__)


def test_stub_all_matches_the_runtime_module():
    tree = stub_tree()
    exported = next(
        node.value
        for node in tree.body
        if isinstance(node, ast.Assign) and node.targets[0].id == "__all__"
    )
    stub_all = {ast.literal_eval(elt) for elt in exported.elts}
    assert stub_all == runtime_public_names()
    stub_classes = {node.name for node in tree.body if isinstance(node, ast.ClassDef)}
    # The module-level functions (`set_memory_limit`, `memory_limit`) are exported too.
    stub_functions = {node.name for node in tree.body if isinstance(node, ast.FunctionDef)}
    assert stub_classes | stub_functions == stub_all


def test_stub_methods_exist_at_runtime_and_vice_versa():
    for node in stub_tree().body:
        if not isinstance(node, ast.ClassDef):
            continue
        runtime = getattr(graphersal, node.name)
        stub_members = {
            item.name
            for item in node.body
            if isinstance(item, ast.FunctionDef) and not item.name.startswith("__")
        }
        runtime_members = {
            name
            for name, _ in inspect.getmembers(runtime)
            if not name.startswith("_") and name not in dir(Exception)
        }
        if issubclass(runtime, Exception):
            runtime_members.discard("help")
        assert stub_members == runtime_members, node.name


def test_every_public_name_has_a_docstring():
    assert graphersal.Graph.__doc__ and graphersal.AccessPolicy.__doc__
    for cls in (graphersal.Graph, graphersal.AccessPolicy):
        for name, member in inspect.getmembers(cls):
            if name.startswith("_"):
                continue
            assert member.__doc__, f"{cls.__name__}.{name}"
    for name in runtime_public_names():
        assert getattr(graphersal, name).__doc__, name
