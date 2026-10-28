extern int pakro_provider_value(void);
int pakro_answer(void) { return pakro_provider_value() + 1; }
