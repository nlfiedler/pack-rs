--
-- Bunch of SQL copied from https://github.com/PackOrganization/Pack and
-- modified to suit various purposes.
--

-- select all rows from item table in breadth-first order
-- result rows: <ignore>, <ignore>, rowid, parent, kind, name
WITH IT AS (SELECT * FROM Item),
    ITI AS (SELECT (ROW_NUMBER() OVER (ORDER BY ID) - 1) AS I, * FROM IT)
    SELECT C.I, IFNULL(P.I, -1) AS PI, C.ID, C.Parent, C.Kind, C.Name FROM ITI AS C
    LEFT JOIN ITI AS P ON C.Parent = P.ID ORDER BY C.I;


-- visit all items in breadth-first order with full paths
WITH RECURSIVE FIT AS (
    SELECT *, Name || IIF(Kind = 1, '/', '') AS Path FROM Item WHERE Parent = 0
    UNION ALL
    SELECT Item.*, FIT.Path || Item.Name || IIF(Item.Kind = 1, '/', '') AS Path
        FROM Item INNER JOIN FIT ON FIT.Kind = 1 AND Item.Parent = FIT.ID
)
SELECT id, parent, kind, Path FROM FIT;


-- select items row(s) by path(s)
-- result rows: <ignore>, <ignore>, rowid, parent, kind, name
WITH RECURSIVE IT AS (
    SELECT Item.*, ID AS FID FROM Item WHERE
    ID IN (
        WITH RECURSIVE FIT AS (
            SELECT *, '/' || Name || IIF(Kind = 1, '/', '') AS Path FROM Item WHERE Parent = 0
            UNION ALL
            SELECT Item.*, FIT.Path || Item.Name || IIF(Item.Kind = 1, '/', '') AS Path
                FROM Item INNER JOIN FIT ON FIT.Kind = 1 AND Item.Parent = FIT.ID
                WHERE '/arch/unix/Makefile.in' LIKE (Path || '%')
                OR '/arch/win32/config.m4' LIKE (Path || '%') -- add a row like this for each addl path
        )
        SELECT ID FROM FIT WHERE Path IN ('/arch/unix/Makefile.in', '/arch/win32/config.m4') -- add paths here
    )
    UNION ALL
    SELECT Item.*, IT.FID FROM Item INNER JOIN IT ON IT.Kind = 1 AND Item.Parent = IT.ID
),
ITI AS (SELECT (ROW_NUMBER() OVER (ORDER BY FID, ID) - 1) AS I, * FROM IT)
SELECT C.I, IFNULL(P.I, -1) AS PI, C.ID, C.Parent, C.Kind, C.Name FROM ITI AS C
LEFT JOIN ITI AS P ON C.FID = P.FID AND C.Parent = P.ID ORDER BY C.I;


-- select item row(s) by id
-- result rows: <ignore>, <ignore>, rowid, parent, kind, name
WITH RECURSIVE IT AS (
    SELECT Item.*, ID AS FID FROM Item WHERE
    ID IN ( 7, 17 )  -- rowid(s) here
    UNION ALL
    SELECT Item.*, IT.FID FROM Item INNER JOIN IT ON IT.Kind = 1 AND Item.Parent = IT.ID
),
ITI AS (SELECT (ROW_NUMBER() OVER (ORDER BY FID, ID) - 1) AS I, * FROM IT)
SELECT C.I, IFNULL(P.I, -1) AS PI, C.ID, C.Parent, C.Kind, C.Name FROM ITI AS C
LEFT JOIN ITI AS P ON C.FID = P.FID AND C.Parent = P.ID ORDER BY C.I;


-- get the total size of the files at the given paths
SELECT IT.PI, IT.ID, Parent, Kind, Name, TOTAL(Size) AS Size FROM (
    WITH RECURSIVE IT AS (
        SELECT Item.*, ID AS FID FROM Item WHERE
        ID IN (
            WITH RECURSIVE FIT AS (
                SELECT *, '/' || Name || IIF(Kind = 1, '/', '') AS Path FROM Item WHERE Parent = 0
                UNION ALL
                SELECT Item.*, FIT.Path || Item.Name || IIF(Item.Kind = 1, '/', '') AS Path
                    FROM Item INNER JOIN FIT ON FIT.Kind = 1 AND Item.Parent = FIT.ID
                    WHERE '/arch/unix/Makefile.in' LIKE (Path || '%')
            )
            SELECT ID FROM FIT WHERE Path IN ('/arch/unix/Makefile.in')
        )
        UNION ALL
        SELECT Item.*, IT.FID FROM Item INNER JOIN IT ON IT.Kind = 1 AND Item.Parent = IT.ID
    ),
    ITI AS (SELECT (ROW_NUMBER() OVER (ORDER BY FID, ID) - 1) AS I, * FROM IT)
    SELECT C.I, IFNULL(P.I, -1) AS PI, C.ID, C.Parent, C.Kind, C.Name FROM ITI AS C
    LEFT JOIN ITI AS P ON C.FID = P.FID AND C.Parent = P.ID ORDER BY C.I
) AS IT LEFT JOIN ItemContent ON IT.ID = ItemContent.Item GROUP BY IT.I;


-- build a temporary index for a particular set of files
CREATE TEMPORARY TABLE IndexedItems (I INTEGER PRIMARY KEY, PI, ID, Kind, Name);
INSERT INTO IndexedItems SELECT I, PI, ID, Kind, Name FROM (
    WITH RECURSIVE IT AS (
        SELECT Item.*, ID AS FID FROM Item WHERE
        ID IN (
            WITH RECURSIVE FIT AS (
                SELECT *, '/' || Name || IIF(Kind = 1, '/', '') AS Path FROM Item WHERE Parent = 0
                UNION ALL
                SELECT Item.*, FIT.Path || Item.Name || IIF(Item.Kind = 1, '/', '') AS Path
                    FROM Item INNER JOIN FIT ON FIT.Kind = 1 AND Item.Parent = FIT.ID
                    WHERE '/httpd-2.4.59/docs/manual/howto/ssi.html.fr.utf8' LIKE (Path || '%')
            )
            SELECT ID FROM FIT WHERE Path IN ('/httpd-2.4.59/docs/manual/howto/ssi.html.fr.utf8')
        )
        UNION ALL
        SELECT Item.*, IT.FID FROM Item INNER JOIN IT ON IT.Kind = 1 AND Item.Parent = IT.ID
    ),
    ITI AS (SELECT (ROW_NUMBER() OVER (ORDER BY FID, ID) - 1) AS I, * FROM IT)
    SELECT C.I, IFNULL(P.I, -1) AS PI, C.ID, C.Parent, C.Kind, C.Name FROM ITI AS C
    LEFT JOIN ITI AS P ON C.FID = P.FID AND C.Parent = P.ID ORDER BY C.I
);
-- use the above index to fetch content related values
SELECT Content, ContentPos, Item, ItemPos, Size FROM IndexedItems
    LEFT JOIN ItemContent ON IndexedItems.Kind = 0 AND IndexedItems.ID = ItemContent.Item
    ORDER BY Content, ContentPos;


-- create a temporary table/index for all files
CREATE TEMPORARY TABLE IndexedFiles (I INTEGER PRIMARY KEY, PI, ID, Kind, Name);
INSERT INTO IndexedFiles SELECT I, PI, ID, Kind, Name FROM (
    WITH RECURSIVE IT AS (
        SELECT Item.*, ID AS FID FROM Item where Kind = 0
    ),
    ITI AS (SELECT (ROW_NUMBER() OVER (ORDER BY FID, ID) - 1) AS I, * FROM IT)
    SELECT C.I, IFNULL(P.I, -1) AS PI, C.ID, C.Parent, C.Kind, C.Name FROM ITI AS C
    LEFT JOIN ITI AS P ON C.FID = P.FID AND C.Parent = P.ID ORDER BY C.I
);
-- use the above index to fetch content related values
SELECT Content, ContentPos, I AS ItemIndex, ItemPos, Size FROM IndexedFiles
    LEFT JOIN ItemContent ON IndexedFiles.Kind = 0 AND IndexedFiles.ID = ItemContent.Item
    ORDER BY Content, ContentPos;
-- example output
-- 1 |      0 |  0 | 0 | 4634
-- 1 |   4634 |  1 | 0 | 2356
-- 1 |   6990 |  2 | 0 | 10058
-- 1 |  17048 |  3 | 0 | 11184
-- 1 |  28232 |  4 | 0 | 241
-- 1 |  28473 |  5 | 0 | 64916
-- 1 |  93389 |  6 | 0 | 183
-- 1 |  93572 |  7 | 0 | 19747
-- 1 | 113319 |  8 | 0 | 13351
-- 1 | 126670 |  9 | 0 | 944
-- 1 | 127614 | 10 | 0 | 3897
-- 1 | 131511 | 11 | 0 | 1114
-- 1 | 132625 | 12 | 0 | 21905
-- 1 | 154530 | 13 | 0 | 41
-- 1 | 154571 | 14 | 0 | 41349
-- 1 | 195920 | 15 | 0 | 2402
-- 1 | 198322 | 16 | 0 | 6899

-- create a temporary table/index of all files with full paths
CREATE TEMPORARY TABLE IndexedFiles (II INTEGER PRIMARY KEY, kind INTEGER, path TEXT);
INSERT INTO IndexedFiles SELECT II, kind, Path FROM (
    WITH RECURSIVE FIT AS (
        SELECT *, Name || IIF(Kind = 1, '/', '') AS Path FROM Item WHERE Parent = 0
        UNION ALL
        SELECT Item.*, FIT.Path || Item.Name || IIF(Item.Kind = 1, '/', '') AS Path
            FROM Item INNER JOIN FIT ON FIT.Kind = 1 AND Item.Parent = FIT.ID
    )
    SELECT id AS II, kind, Path FROM FIT WHERE kind <> 1
);
-- use the above index to fetch content related values
-- add a select on II as ItemId to then batch writes based on the item
-- (that is, for any particular item, batch all of the operations into a single task)
SELECT content, contentpos, itempos, Size, kind, Path FROM IndexedFiles
    LEFT JOIN itemcontent ON IndexedFiles.II = ItemContent.Item
    ORDER BY content, contentpos;
-- exmaple output
-- 1 |      0 | 0 |     0 | 0 | arch/empty_file.txt
-- 1 |      0 | 0 |     4 | 2 | arch/link_to_unix
-- 1 |      4 | 0 |  4634 | 0 | arch/win32/mod_isapi.dsp
-- 1 |   4638 | 0 |  2356 | 0 | arch/win32/mod_isapi.dep
-- 1 |   6994 | 0 | 10058 | 0 | arch/win32/mod_isapi.mak
-- 1 |  17052 | 0 | 11184 | 0 | arch/win32/mod_isapi.h
-- 1 |  28236 | 0 |   241 | 0 | arch/win32/config.m4
-- 1 |  28477 | 0 | 64916 | 0 | arch/win32/mod_isapi.c
-- 1 |  93393 | 0 |   183 | 0 | arch/win32/Makefile.in
-- 1 |  93576 | 0 | 19747 | 0 | arch/win32/mod_win32.c
-- 1 | 113323 | 0 | 13351 | 0 | arch/unix/mod_unixd.c
-- 1 | 126674 | 0 |   944 | 0 | arch/unix/config5.m4
-- 1 | 127618 | 0 |  3897 | 0 | arch/unix/mod_systemd.c
-- 1 | 131515 | 0 |  1114 | 0 | arch/unix/mod_unixd.h
-- 1 | 132629 | 0 | 21905 | 0 | arch/unix/mod_privileges.c
-- 1 | 154534 | 0 |    41 | 0 | arch/unix/Makefile.in
-- 1 | 154575 | 0 | 41349 | 0 | arch/netware/mod_nw_ssl.c
-- 1 | 195924 | 0 |  2402 | 0 | arch/netware/libprews.c
-- 1 | 198326 | 0 |  6899 | 0 | arch/netware/mod_netware.c
