# 由 acme:notes/tool@0.1.0 的 WIT 接口生成。

class Hit:
    path: str
    line: int
    text: str

class ReadDir:
    def grep(self, needle: str, max_hits: int | None = None) -> list[Hit]:
        ...
    def forge(self) -> None:
        ...

class WriteDir(ReadDir):
    def append_note(self, path: str, text: str) -> int:
        ...

class ExecEnv:
    ...

class Egress:
    def fetch_into(self, root: WriteDir, url: str, path: str) -> int:
        ...

