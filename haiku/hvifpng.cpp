// Renders a Haiku vector icon exactly the way Tracker and Deskbar do, to a
// PNG — the check that haiku/haikucode.hvif is a real icon and not just bytes.
// Run ON Haiku:
//   g++ -O1 -o hvifpng haiku/hvifpng.cpp -lbe -ltranslation
//   ./hvifpng --icon haiku/haikucode.hvif out.png 256      (a bare HVIF file)
//   ./hvifpng "$HOME/config/apps/Haiku Code" out.png 128   (a file's BEOS:ICON)
// A PNG of the rendered icon and "No error" from both steps is a pass.
#include <Application.h>
#include <Bitmap.h>
#include <BitmapStream.h>
#include <File.h>
#include <IconUtils.h>
#include <NodeInfo.h>
#include <TranslatorRoster.h>
#include <fs_attr.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void printCode(const char* what, type_code code)
{
	printf("%s '%c%c%c%c'\n", what, code >> 24, (code >> 16) & 0xff, (code >> 8) & 0xff,
		code & 0xff);
}

// The icon data of `path`: a bare .hvif, or whatever BEOS:ICON a file carries.
static status_t load(const char* path, bool bare, uint8** data, size_t* size, type_code* code)
{
	if (bare) {
		BFile file(path, B_READ_ONLY);
		status_t status = file.InitCheck();
		if (status != B_OK)
			return status;
		off_t length = 0;
		file.GetSize(&length);
		*data = (uint8*)malloc(length);
		*size = length;
		*code = 'VICN';
		return file.Read(*data, length) == length ? B_OK : B_IO_ERROR;
	}
	BFile file(path, B_READ_ONLY);
	status_t status = file.InitCheck();
	if (status != B_OK)
		return status;
	BNodeInfo info(&file);
	char mime[B_MIME_TYPE_LENGTH] = "";
	if (info.GetType(mime) == B_OK)
		printf("file type: %s\n", mime);
	attr_info attrs;
	ssize_t attrSize = file.GetAttrInfo("BEOS:ICON", &attrs);
	if (attrSize < 0)
		return (status_t)attrSize;
	printCode("BEOS:ICON:", attrs.type);
	// packagefs reports 0 for packaged files' attributes; GetIcon still loads them.
	if (attrSize > 0)
		printf("BEOS:ICON: %ld bytes\n", (long)attrSize);
	return info.GetIcon(data, size, code);
}

int main(int argc, char** argv)
{
	bool bare = false;
	int arg = 1;
	if (argc > 1 && strcmp(argv[1], "--icon") == 0) {
		bare = true;
		arg = 2;
	}
	if (argc < arg + 3) {
		fprintf(stderr, "usage: hvifpng [--icon] <file|app> <out.png> <size>\n");
		return 2;
	}
	const int size = atoi(argv[arg + 2]);
	BApplication app("application/x-vnd.haikucode-hvifcheck");

	uint8* data = NULL;
	size_t length = 0;
	type_code code = 0;
	status_t status = load(argv[arg], bare, &data, &length, &code);
	if (status != B_OK) {
		printf("no icon in %s -> %s\n", argv[arg], strerror(status));
		return 1;
	}
	printf("icon data: %ld bytes\n", (long)length);

	BBitmap bitmap(BRect(0, 0, size - 1, size - 1), B_RGBA32);
	status = BIconUtils::GetVectorIcon(data, length, &bitmap);
	printf("render at %dpx -> %s\n", size, strerror(status));
	free(data);
	if (status != B_OK)
		return 1;

	BTranslatorRoster* roster = BTranslatorRoster::Default();
	BBitmapStream stream(&bitmap);
	BFile out(argv[arg + 1], B_CREATE_FILE | B_ERASE_FILE | B_WRITE_ONLY);
	status = roster->Translate(&stream, NULL, NULL, &out, B_PNG_FORMAT);
	BBitmap* detached = NULL;
	stream.DetachBitmap(&detached);
	printf("wrote %s -> %s\n", argv[arg + 1], strerror(status));
	return status == B_OK ? 0 : 1;
}
