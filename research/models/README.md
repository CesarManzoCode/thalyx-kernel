# Modelos finitos de investigación

Estos programas comprueban consecuencias de **dos abstracciones pequeñas del diseño**. No son código de kernel, un simulador de hardware ni una verificación formal de toda la arquitectura.

Ejecutar desde la raíz del repositorio con Python 3.10 o posterior:

```sh
python3 research/models/check_models.py
python3 research/models/check_models.py --output research/models/results.json
```

El segundo comando guarda un informe reproducible con hash del script y versión del intérprete. El resultado fundacional está en [results.json](results.json).

| Modelo | Espacio explorado | Propiedad y control negativo |
|---|---|---|
| MODEL-01 | Un grant padre/derivación, un ámbito y dos invocaciones; cola, recepción, comprobación, admisión de efecto, cierre y resolución intercalados. | No admite después de barrera ni retira tickets pendientes. Una variante que separa check/admisión debe admitir indebidamente tras fence. También encuentra un efecto legítimo posterior a fence cuando se admitió antes. |
| MODEL-02 | Un PREPARE, un objeto de dos fragmentos, un COMMIT de dos fragmentos, flushes, ACK y todos los subconjuntos persistidos posibles entre pasos. | Un commit completo no precede a dependencias durables; ACK sobrevive. Quitar flush de datos o responder demasiado pronto debe producir contraejemplo. |
| Caso ABA | Publicaciones A/0 → B/1 → A/2. | Comparar solo contenido acepta incorrectamente una expectativa antigua; comparar generación la rechaza. |

MODEL-01 no modela MMU, DMA, memoria débil, múltiples grants ancestrales, clocks ni servidores maliciosos. «Retirar» solo considera los tickets abstractos del modelo. MODEL-02 abstrae checksums como validez de fragmentos y presupone flush correcto; no prueba detección de corrupción, GC, formato de disco, dedupe completo ni el comportamiento de una controladora.

Los controles negativos deben fallar en el sentido esperado; si no encuentran contraejemplo, el script termina con error. Un número de estados explorados no es porcentaje de seguridad ni cobertura de implementación.
