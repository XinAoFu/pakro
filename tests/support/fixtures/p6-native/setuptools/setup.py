import os
from setuptools import Extension, setup

setup(
    name="p6-setuptools-native",
    version="1.0.0",
    ext_modules=[Extension("p6_native_demo", ["src/demo.c"],
                           define_macros=[("P6_NONCE", '"' + os.urandom(16).hex() + '"')])],
)
