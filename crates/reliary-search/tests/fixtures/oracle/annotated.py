"""Oracle fixture: Python defs incl. annotations (B4)."""
import functools


def plain(x):
    return x


def annotated(x) -> int:
    return x


async def aworker(name) -> list[str]:
    return [name]


def unioned(x) -> int | None:
    return x


class Widget:
    def method(self, x):
        return x

    @classmethod
    def make(cls, name):
        return cls(name)


@functools.lru_cache(maxsize=None)
def cached(x):
    return x
