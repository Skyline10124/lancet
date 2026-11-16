# 由 acme:notes/tool@0.1.0 的 WIT 接口生成。

class Hit:
    """一处匹配。"""
    path: str
    """相对路径。"""
    line: int
    """行号，从 1 开始。"""
    text: str
    """该行文本。"""

class ReadDir:
    def grep(self, needle: str, max_hits: int | None = None) -> list[Hit]:
        """在目录中按子串搜索，最多返回 max-hits 条。"""
    def forge(self) -> None:
        """测试用：把只读句柄的编号当作可写句柄使用。"""

class WriteDir(ReadDir):
    def append_note(self, path: str, text: str) -> int:
        """在文件末尾追加一行，返回追加后的行数。"""

class ExecEnv:
    ...

class Egress:
    def fetch_into(self, root: WriteDir, url: str, path: str) -> int:
        """经出口抓取 URL，写入目录中的文件，返回字节数。"""

