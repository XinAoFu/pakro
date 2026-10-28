#include <Python.h>
static PyObject *answer(PyObject *, PyObject *) { return PyLong_FromLong(42); }
static PyMethodDef methods[] = {{"answer", answer, METH_NOARGS, nullptr}, {nullptr, nullptr, 0, nullptr}};
static struct PyModuleDef module = {PyModuleDef_HEAD_INIT, "p6_native_demo", nullptr, -1, methods};
PyMODINIT_FUNC PyInit_p6_native_demo(void) { return PyModule_Create(&module); }
