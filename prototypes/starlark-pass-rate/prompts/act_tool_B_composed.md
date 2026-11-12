Run a Starlark script in the repository. The script returns one value, which
is shown to you as JSON. Keep it under 64 KiB: filter large text before
returning it.

## Outcome

- The script ends normally (by `return` or by reaching the end): all file
  changes are **kept**, and the returned value is shown to you.
- `discard(value)`: stops the script immediately, **throws away** all file
  changes made in this call, and shows `value` to you.
- `fail(message)`, or any error: stops the script, throws away all file
  changes, and shows you the error with its line number.

There is no `try`/`except`. An error always stops the script.

## Returning a value

Call `result(value)` to set the value that is shown to you. Call it at most
once; a second call is an error. A script that never calls `result` returns
`None`.

`return` is only allowed inside a `def`. A `return` at the top level of the
script is a syntax error.

## Repository: `repo`

Paths are relative to the repository root.

| Call | Result |
| --- | --- |
| `repo.read(path, missing_ok = False)` | File content as a string. If the file does not exist: returns `None` when `missing_ok = True`, otherwise an error. |
| `repo.list(path = ".")` | List of entry names in a directory. Directory names end with `/`. |
| `repo.search(pattern, glob = "**")` | Regex search over file contents. Returns a list of results with fields `.path`, `.line` (1-based int) and `.text`. |
| `repo.write(path, content)` | Create or overwrite a file. |
| `repo.edit(path, old, new, all = False)` | Replace text. With `all = False`, `old` must occur **exactly once** in the file, otherwise an error. With `all = True`, every occurrence is replaced; `old` must occur at least once. |
| `repo.reset(path)` | Undo this call's changes to `path`. |
| `repo.status()` | Files changed so far in this call: list of results with `.path` and `.kind` (`"added"`, `"modified"`, `"deleted"`). |
| `repo.diff()` | Unified diff of all changes so far in this call, as a string. |

## Commands: `env`

Commands are argument lists. There is no shell: `|`, `>`, `&&` and `*` are
passed literally.

| Call | Result |
| --- | --- |
| `env.run(argv, check = True)` | Runs one command in the repository root. Returns a result with `.code`, `.stdout`, `.stderr`. With `check = True`, a non-zero exit code is an error. With `check = False`, you get the result and decide yourself. |
| `env.pipe([argv1, argv2, ...], check = True)` | Runs commands connected by pipes. Same result fields as `env.run`, for the last command. |

## Text: `text(s)`

`text(s)` wraps a string so you can filter it. Each step returns a new
filtered view; `str(view)` gives the string back.

| Call | Result |
| --- | --- |
| `.grep(pattern, before = 0, after = 0)` | Lines matching the regex, with context lines. |
| `.head(n)`, `.tail(n)` | First or last `n` lines. |
| `.lines()` | List of line strings. |

`json.encode(value)` and `json.decode(string)` are available.

## Starlark is not Python

| Python | Starlark |
| --- | --- |
| `while` loops | Not available. Use `for` over a list or `range(n)`. |
| `try` / `except`, `raise` | Not available. Use `missing_ok`, `check = False`, or `fail(message)`. |
| `import`, `class`, `with`, `yield`, `global`, `assert` | Not available. |
| f-strings `f"{x}"` | Not available. Use `"%s: %d" % (name, count)` or `"{}".format(x)`. |
| `x is None` | Use `x == None`. |
| `for ch in some_string` | Strings are not iterable. Use `some_string.elems()`. |
| `a < b < c` | Not allowed. Write `a < b and b < c`. |
| recursion | Allowed in this interpreter. |
| `set()` | Not available. Use a dict with `True` values. |
| `print(x)` | Not available in this environment. Return what you need. |

`def`, `lambda`, list and dict comprehensions, `if`/`elif`/`else`, `for`,
`break`, `continue`, `len`, `sorted`, `enumerate`, `zip`, `min`, `max`,
`any`, `all` and the usual string methods work as in Python.

## Example

```python
path = "src/config.rs"
content = repo.read(path)
old = "pub const MAX_ITEMS: usize = 10;"
new = "pub const MAX_ITEMS: usize = 20;"
if old not in content:
    discard({"reason": "expected setting not found"})
repo.edit(path, old, new)
tests = env.run(["cargo", "test"], check = False)
if tests.code != 0:
    discard({"reason": tests.stdout + tests.stderr})
result({"summary": tests.stdout, "diff": repo.diff()})
```
