/// A file to open: what the viewers need to show it and step to the next one.
class FileRef {
  const FileRef({required this.id, required this.name, this.size = 0, this.mtime = 0});
  final int id;
  final String name;
  final int size;
  final int mtime;
}
