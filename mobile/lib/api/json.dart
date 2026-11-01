/// Reading the service's JSON without trusting its types: a missing or odd value reads as the
/// default instead of throwing, so a newer or older service never takes a whole screen down.
library;

typedef Json = Map<String, Object?>;

Json asJson(Object? v) => v is Map ? v.cast<String, Object?>() : const {};

List<Json> asJsonList(Object? v) => [
  if (v is List)
    for (final x in v)
      if (x is Map) x.cast<String, Object?>(),
];

int? _int(Object? v) => switch (v) {
  int i => i,
  num n => n.toInt(),
  String s => int.tryParse(s),
  _ => null,
};

extension JsonRead on Json {
  int integer(String k, [int fallback = 0]) => _int(this[k]) ?? fallback;
  int? intOrNull(String k) => _int(this[k]);
  double? number(String k) => switch (this[k]) {
    num n => n.toDouble(),
    String s => double.tryParse(s),
    _ => null,
  };
  String str(String k, [String fallback = '']) => strOrNull(k) ?? fallback;
  String? strOrNull(String k) => switch (this[k]) {
    String s => s,
    null => null,
    Object o => o.toString(),
  };
  bool flag(String k) => this[k] == true || this[k] == 1;
  bool? flagOrNull(String k) => switch (this[k]) {
    bool b => b,
    _ => null,
  };
  Json obj(String k) => asJson(this[k]);
  Json? objOrNull(String k) => this[k] is Map ? asJson(this[k]) : null;
  List<Json> list(String k) => asJsonList(this[k]);
  List<String> strs(String k) => [
    if (this[k] is List)
      for (final x in this[k] as List)
        if (x != null) x.toString(),
  ];
  List<int> ints(String k) => [
    if (this[k] is List)
      for (final x in this[k] as List) ?_int(x),
  ];
}
