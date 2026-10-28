#include <Python.h>
static PyObject *answer(PyObject *self, PyObject *unused) { (void)self; (void)unused; return PyLong_FromLong(42); }
static PyMethodDef methods[] = {{"answer", answer, METH_NOARGS, NULL}, {NULL, NULL, 0, NULL}};
static struct PyModuleDef module = {PyModuleDef_HEAD_INIT, "p6_native_demo", NULL, -1, methods};
PyMODINIT_FUNC PyInit_p6_native_demo(void) { return PyModule_Create(&module); }
