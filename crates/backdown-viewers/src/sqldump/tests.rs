use super::*;
use std::io::Write;

fn tmp(bytes: &[u8], suffix: &str) -> tempfile::NamedTempFile {
    let mut f = tempfile::Builder::new().suffix(suffix).tempfile().unwrap();
    f.write_all(bytes).unwrap();
    f.flush().unwrap();
    f
}

/// Read all of `sql` in one go.
fn read(sql: &[u8]) -> Value {
    let f = tmp(sql, ".sql");
    let mut p = Parser::open(f.path()).unwrap();
    assert_eq!(p.not_sql(), None);
    assert!(p.run(Instant::now() + Duration::from_secs(60)));
    p.json(true)
}

fn table<'a>(v: &'a Value, key: &str) -> &'a Value {
    v["tables"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["key"] == key)
        .unwrap_or_else(|| panic!("no table {key} in {:#}", v["tables"]))
}

fn cols(t: &Value) -> Vec<String> {
    t["columns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap().to_string())
        .collect()
}

fn fact(v: &Value, k: &str) -> Option<String> {
    v["facts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f[0] == k)
        .map(|f| f[1].as_str().unwrap().to_string())
}

fn names(v: &Value, list: &str) -> Vec<String> {
    v[list]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["name"].as_str().unwrap().to_string())
        .collect()
}

const MYSQL: &str = r#"-- MySQL dump 10.13  Distrib 8.0.36, for Linux (x86_64)
--
-- Host: db.example.org    Database: shop
-- ------------------------------------------------------
-- Server version	8.0.36

/*!40101 SET @OLD_CHARACTER_SET_CLIENT=@@CHARACTER_SET_CLIENT */;
/*!50503 SET NAMES utf8mb4 */;
/*!40103 SET @OLD_TIME_ZONE=@@TIME_ZONE */;
/*!40014 SET @OLD_FOREIGN_KEY_CHECKS=@@FOREIGN_KEY_CHECKS, FOREIGN_KEY_CHECKS=0 */;
/*!40101 SET @OLD_SQL_MODE=@@SQL_MODE, SQL_MODE='NO_AUTO_VALUE_ON_ZERO' */;

--
-- Table structure for table `users`
--

DROP TABLE IF EXISTS `users`;
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!50503 SET character_set_client = utf8mb4 */;
CREATE TABLE `users` (
  `id` int unsigned NOT NULL AUTO_INCREMENT,
  `email` varchar(255) COLLATE utf8mb4_unicode_ci NOT NULL,
  `name` varchar(100) DEFAULT NULL COMMENT 'shown; to others',
  `created_at` timestamp NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
  PRIMARY KEY (`id`),
  UNIQUE KEY `email` (`email`),
  KEY `idx_name` (`name`(20))
) ENGINE=InnoDB AUTO_INCREMENT=5 DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci COMMENT='people';
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table `users`
--

LOCK TABLES `users` WRITE;
/*!40000 ALTER TABLE `users` DISABLE KEYS */;
INSERT INTO `users` VALUES (1,'ann@example.org','Ann (admin); \'the\' boss','2024-01-02 03:04:05'),(2,'bob@example.org','Bob \\','2024-01-02 03:04:05'),(3,'cy@example.org','it''s ) (( ;',NULL);
INSERT INTO `users` VALUES (4,'di@example.org',"double \" quoted ; )",NULL);
/*!40000 ALTER TABLE `users` ENABLE KEYS */;
UNLOCK TABLES;

DROP TABLE IF EXISTS `orders`;
CREATE TABLE `orders` (
  `id` bigint NOT NULL,
  `user_id` int unsigned NOT NULL,
  `total` decimal(10,2) NOT NULL DEFAULT '0.00',
  `note` text,
  PRIMARY KEY (`id`),
  KEY `fk_user` (`user_id`),
  CONSTRAINT `fk_user` FOREIGN KEY (`user_id`) REFERENCES `users` (`id`) ON DELETE CASCADE ON UPDATE RESTRICT
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

LOCK TABLES `orders` WRITE;
INSERT INTO `orders` VALUES (10,1,9.99,_binary 'a\0b'),(11,2,0.00,'x\'),(y'),(12,1,1.50,NULL) ON DUPLICATE KEY UPDATE total=VALUES(total);
UNLOCK TABLES;

--
-- Temporary view structure for view `v_big`
--

DROP TABLE IF EXISTS `v_big`;
/*!50001 DROP VIEW IF EXISTS `v_big`*/;
/*!50001 CREATE VIEW `v_big` AS SELECT
 1 AS `id`*/;

/*!50003 SET @saved_cs_client      = @@character_set_client */ ;
DELIMITER ;;
/*!50003 CREATE*/ /*!50017 DEFINER=`root`@`localhost`*/ /*!50003 TRIGGER `orders_bi` BEFORE INSERT ON `orders` FOR EACH ROW BEGIN
  IF NEW.total < 0 THEN SET NEW.total = 0; END IF;
  INSERT INTO audit VALUES (1), (2);
END */;;
DELIMITER ;

DELIMITER ;;
CREATE DEFINER=`root`@`localhost` PROCEDURE `cleanup`(IN days INT)
BEGIN
  DELETE FROM orders WHERE id < days; -- not the end;
  SELECT 'done;';
END ;;
DELIMITER ;

/*!50001 DROP VIEW IF EXISTS `v_big`*/;
/*!50001 CREATE ALGORITHM=UNDEFINED */
/*!50013 DEFINER=`root`@`localhost` SQL SECURITY DEFINER */
/*!50001 VIEW `v_big` AS select `orders`.`id` AS `id` from `orders` where (`orders`.`total` > 5) */;

/*!40111 SET SQL_NOTES=@OLD_SQL_NOTES */;

-- Dump completed on 2024-01-15 10:22:33
"#;

#[test]
fn mysqldump_schema_rows_and_header() {
    let v = read(MYSQL.as_bytes());
    assert_eq!(v["dialect"], "mysql");
    assert_eq!(v["truncated"], false, "{:#}", v["notes"]);
    assert_eq!(
        fact(&v, "Made by").unwrap(),
        "mysqldump 10.13 (with MySQL 8.0.36)"
    );
    assert_eq!(fact(&v, "Host").unwrap(), "db.example.org");
    assert_eq!(fact(&v, "Database").unwrap(), "shop");
    assert_eq!(fact(&v, "Server").unwrap(), "8.0.36");
    assert_eq!(fact(&v, "Dumped").unwrap(), "2024-01-15 10:22:33");
    assert_eq!(fact(&v, "Character set").unwrap(), "utf8mb4");

    let users = table(&v, "shop.users");
    assert_eq!(cols(users), ["id", "email", "name", "created_at"]);
    assert_eq!(users["rows"], 4);
    assert_eq!(users["statements"], 2);
    assert_eq!(users["engine"], "InnoDB");
    assert_eq!(users["comment"], "people");
    assert_eq!(users["primary_key"], json!(["id"]));
    let c = &users["columns"];
    assert_eq!(c[0]["type"], "int unsigned");
    assert_eq!(c[0]["auto"], true);
    assert_eq!(c[0]["null"], false);
    assert_eq!(c[1]["type"], "varchar(255)");
    assert_eq!(c[2]["null"], true);
    assert_eq!(c[2]["default"], "NULL");
    assert_eq!(c[2]["comment"], "shown; to others");
    assert_eq!(c[3]["default"], "CURRENT_TIMESTAMP");
    assert_eq!(c[3]["extra"], json!(["on update CURRENT_TIMESTAMP"]));
    let idx = users["indexes"].as_array().unwrap();
    assert_eq!(idx.len(), 2);
    assert_eq!(idx[0]["kind"], "unique");
    assert_eq!(idx[1]["columns"], json!(["name(20)"]));

    let orders = table(&v, "shop.orders");
    assert_eq!(
        orders["rows"], 3,
        "ON DUPLICATE KEY UPDATE … VALUES(…) isn't a row"
    );
    let fk = &orders["foreign_keys"][0];
    assert_eq!(fk["ref"], "shop.users");
    assert_eq!(fk["columns"], json!(["user_id"]));
    assert_eq!(fk["ref_columns"], json!(["id"]));
    assert_eq!(fk["on_delete"], "cascade");
    assert_eq!(fk["on_update"], "restrict");
    assert_eq!(orders["columns"][2]["default"], "'0.00'");

    // the trigger body's INSERT isn't data, and there's no table called audit
    assert!(
        v["tables"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["name"] != "audit")
    );
    assert_eq!(names(&v, "triggers"), ["orders_bi"]);
    assert_eq!(v["triggers"][0]["on"], "shop.orders");
    assert_eq!(names(&v, "routines"), ["cleanup"]);
    assert_eq!(
        names(&v, "views"),
        ["v_big"],
        "the stand-in and the view are one view"
    );
    assert_eq!(v["tables"].as_array().unwrap().len(), 2);
}

#[test]
fn mariadb_sandbox_line_and_executable_comments() {
    let dump = "/*M!999999\\- enable the sandbox mode */ \n-- MariaDB dump 10.19  Distrib 10.11.6-MariaDB, for debian-linux-gnu (x86_64)\n--\n-- Host: localhost    Database: wiki\n\n/*!40101 SET NAMES utf8mb4 */;\nCREATE TABLE `page` (`id` int(10) unsigned NOT NULL, `title` varbinary(255) NOT NULL DEFAULT '', PRIMARY KEY (`id`)) ENGINE=InnoDB;\nINSERT INTO `page` VALUES (1,'Main_Page'),(2,'Talk:\\'x\\'');\n-- Dump completed on 2024-02-03 10:00:00\n";
    let v = read(dump.as_bytes());
    assert_eq!(
        fact(&v, "Made by").unwrap(),
        "mariadb-dump 10.19 (with MariaDB 10.11.6)"
    );
    let page = table(&v, "wiki.page");
    assert_eq!(page["rows"], 2);
    assert_eq!(page["columns"][1]["default"], "''");
    assert_eq!(v["truncated"], false);
}

const PG: &str = r#"--
-- PostgreSQL database dump
--

\restrict AbCdEf123

-- Dumped from database version 16.1 (Debian 16.1-1.pgdg120+1)
-- Dumped by pg_dump version 16.1 (Debian 16.1-1.pgdg120+1)

SET statement_timeout = 0;
SET client_encoding = 'UTF8';
SET standard_conforming_strings = on;
SELECT pg_catalog.set_config('search_path', '', false);

CREATE SCHEMA sales;

CREATE FUNCTION public.touch() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
  NEW.updated := now(); -- a ; inside
  RAISE NOTICE 'it''s done; really';
  RETURN NEW;
END;
$$;

CREATE FUNCTION public.answer() RETURNS integer
LANGUAGE sql
BEGIN ATOMIC
 SELECT 42;
END;

CREATE TABLE public.users (
id integer NOT NULL,
email character varying(255) NOT NULL,
bio text DEFAULT 'C:\'::text,
created timestamp with time zone DEFAULT now()
);

COMMENT ON TABLE public.users IS 'People who can sign in';
COMMENT ON COLUMN public.users.email IS 'unique; lowercased';

CREATE SEQUENCE public.users_id_seq
AS integer
START WITH 1
INCREMENT BY 1
CACHE 1;

ALTER TABLE ONLY public.users ALTER COLUMN id SET DEFAULT nextval('public.users_id_seq'::regclass);

CREATE TABLE sales.orders (
id bigint NOT NULL,
user_id integer,
amount numeric(10,2)
);

CREATE VIEW public.big_orders AS
 SELECT id FROM sales.orders WHERE (amount > (100)::numeric);

COPY public.users (id, email, bio, created) FROM stdin;
1	ann@example.org	line one\nline two	2024-01-02 03:04:05+00
2	bob@example.org	\N	2024-01-02 03:04:05+00
3	cy@example.org	tab\there; not a statement	\N
\.

COPY sales.orders (id, user_id, amount) FROM stdin;
10	1	5.00
11	2	150.00
\.

ALTER TABLE ONLY public.users
ADD CONSTRAINT users_pkey PRIMARY KEY (id);

ALTER TABLE ONLY sales.orders
ADD CONSTRAINT orders_pkey PRIMARY KEY (id);

CREATE UNIQUE INDEX users_email_key ON public.users USING btree (lower((email)::text));
CREATE INDEX orders_user ON sales.orders USING btree (user_id);

CREATE TRIGGER users_touch BEFORE UPDATE ON public.users FOR EACH ROW EXECUTE FUNCTION public.touch();

ALTER TABLE ONLY sales.orders
ADD CONSTRAINT orders_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE SET NULL;

\unrestrict AbCdEf123

--
-- PostgreSQL database dump complete
--

"#;

#[test]
fn pg_dump_copy_blocks_and_late_constraints() {
    let v = read(PG.as_bytes());
    assert_eq!(v["dialect"], "postgres");
    assert_eq!(v["truncated"], false, "{:#}", v["notes"]);
    assert_eq!(
        fact(&v, "Made by").unwrap(),
        "pg_dump 16.1 (Debian 16.1-1.pgdg120+1)"
    );
    assert_eq!(
        fact(&v, "Server").unwrap(),
        "PostgreSQL 16.1 (Debian 16.1-1.pgdg120+1)"
    );
    assert_eq!(fact(&v, "Character set").unwrap(), "UTF8");
    let users = table(&v, "public.users");
    assert_eq!(users["rows"], 3);
    assert_eq!(users["copy"], true);
    assert_eq!(cols(users), ["id", "email", "bio", "created"]);
    assert_eq!(users["columns"][0]["auto"], true);
    assert_eq!(users["columns"][0]["pk"], true);
    assert_eq!(users["columns"][1]["type"], "character varying(255)");
    assert_eq!(users["columns"][1]["comment"], "unique; lowercased");
    assert_eq!(users["columns"][2]["default"], "'C:\\'::text");
    assert_eq!(users["columns"][3]["type"], "timestamp with time zone");
    assert_eq!(users["comment"], "People who can sign in");
    assert_eq!(users["indexes"][0]["kind"], "unique");
    assert_eq!(
        users["indexes"][0]["columns"],
        json!(["lower((email)::text)"])
    );
    let orders = table(&v, "sales.orders");
    assert_eq!(orders["rows"], 2);
    assert_eq!(orders["primary_key"], json!(["id"]));
    assert_eq!(orders["foreign_keys"][0]["ref"], "public.users");
    assert_eq!(orders["foreign_keys"][0]["on_delete"], "set null");
    assert_eq!(v["schemas"], json!(["sales", "public"]));
    assert_eq!(names(&v, "routines"), ["touch", "answer"]);
    assert_eq!(names(&v, "views"), ["big_orders"]);
    assert_eq!(names(&v, "sequences"), ["users_id_seq"]);
    assert_eq!(v["triggers"][0]["on"], "public.users");
    assert_eq!(v["tables"].as_array().unwrap().len(), 2);
}

#[test]
fn pg_inserts_with_every_kind_of_quoting() {
    let dump = r#"--
-- PostgreSQL database dump
--
SET standard_conforming_strings = on;
CREATE TABLE public.t (id integer, s text);
INSERT INTO public.t VALUES (1, 'plain \ backslash');
INSERT INTO public.t VALUES (2, E'escaped \' quote; (');
INSERT INTO public.t VALUES (3, 'x'), (4, $$dollar ' ; ( quoted$$), (5, /* a ) comment; */ 'y'), (6, $q$ $$ ; $q$);
SET standard_conforming_strings = off;
INSERT INTO public.t (id, s) VALUES (7, 'now \' escapes ;');
INSERT INTO public.t VALUES (8, 'a') ON CONFLICT (id) DO NOTHING;
--
-- PostgreSQL database dump complete
--
"#;
    let v = read(dump.as_bytes());
    assert_eq!(v["truncated"], false, "{:#}", v["notes"]);
    assert_eq!(table(&v, "public.t")["rows"], 8);
    assert_eq!(table(&v, "public.t")["statements"], 5);
}

#[test]
fn sqlite_dump_with_standard_strings_and_a_trigger() {
    let dump = r#"PRAGMA foreign_keys=OFF;
BEGIN TRANSACTION;
CREATE TABLE orgs(id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE);
INSERT INTO orgs VALUES(1,'Acme ''Tools''');
INSERT INTO orgs VALUES(2,'C:\');
INSERT INTO orgs VALUES(3,'back\''slash');
CREATE TABLE kv(key TEXT, value TEXT, org INTEGER REFERENCES orgs(id), PRIMARY KEY(key, org));
INSERT INTO kv VALUES('a','(1);',1);
INSERT INTO "kv" VALUES('b','2',2);
DELETE FROM sqlite_sequence;
INSERT INTO sqlite_sequence VALUES('orgs',3);
CREATE INDEX kv_org ON kv(org);
CREATE TRIGGER orgs_log AFTER INSERT ON orgs BEGIN
  INSERT INTO kv VALUES('log', CASE WHEN new.id > 1 THEN 'x' ELSE 'y' END, new.id);
  UPDATE kv SET value = 'z';
END;
CREATE VIEW names AS SELECT name FROM orgs;
COMMIT;
"#;
    let v = read(dump.as_bytes());
    assert_eq!(v["dialect"], "sqlite");
    assert_eq!(fact(&v, "Made by").unwrap(), "sqlite3 .dump");
    assert_eq!(v["truncated"], false, "{:#}", v["notes"]);
    let orgs = table(&v, "orgs");
    assert_eq!(orgs["rows"], 3, "'C:\\' ends its string in SQLite");
    assert_eq!(orgs["columns"][0]["auto"], true);
    assert_eq!(orgs["columns"][1]["null"], false);
    let kv = table(&v, "kv");
    assert_eq!(kv["rows"], 2, "the trigger's INSERT isn't a row");
    assert_eq!(cols(kv), ["key", "value", "org"]);
    assert_eq!(kv["primary_key"], json!(["key", "org"]));
    assert_eq!(kv["foreign_keys"][0]["ref"], "orgs");
    assert_eq!(kv["indexes"][0]["name"], "kv_org");
    assert_eq!(names(&v, "triggers"), ["orgs_log"]);
    assert_eq!(names(&v, "views"), ["names"]);
}

#[test]
fn generic_script_without_a_header() {
    let dump = "create table t (id int primary key, label varchar(20) default 'a,b', parent int, foreign key (parent) references t(id));\n\
        insert into t (id, label) values (1, 'one (1)'), (2, 'two, 2'),\n  (3, 'three');\n\
        insert into t select * from t;\n\
        INSERT INTO t SET id = 9, label = 'x';\n\
        insert into t values (4, 'no semicolon at the end')";
    let v = read(dump.as_bytes());
    let t = table(&v, "t");
    assert_eq!(t["rows"], 5);
    assert_eq!(t["uncounted"], true);
    assert_eq!(t["columns"][1]["default"], "'a,b'");
    assert_eq!(t["primary_key"], json!(["id"]));
    assert_eq!(t["foreign_keys"][0]["ref"], "t");
    assert_eq!(v["tool"], Value::Null);
}

#[test]
fn phpmyadmin_keys_come_in_alter_statements() {
    let dump = "-- phpMyAdmin SQL Dump\n-- version 5.2.1\n-- https://www.phpmyadmin.net/\n--\n-- Host: 127.0.0.1\n-- Generation Time: Jan 15, 2024 at 10:22 AM\n-- Server version: 10.4.32-MariaDB\n-- PHP Version: 8.2.12\n\nSET SQL_MODE = \"NO_AUTO_VALUE_ON_ZERO\";\n\n--\n-- Database: `shop`\n--\n\nCREATE TABLE `users` (\n  `id` int(11) NOT NULL,\n  `email` varchar(255) NOT NULL\n) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;\n\nINSERT INTO `users` (`id`, `email`) VALUES\n(1, 'a@example.org'),\n(2, 'b@example.org');\n\nCREATE TABLE `posts` (`id` int(11) NOT NULL, `user_id` int(11) NOT NULL) ENGINE=InnoDB;\n\nALTER TABLE `users`\n  ADD PRIMARY KEY (`id`),\n  ADD UNIQUE KEY `email` (`email`);\n\nALTER TABLE `posts`\n  ADD PRIMARY KEY (`id`),\n  ADD KEY `user_id` (`user_id`);\n\nALTER TABLE `users`\n  MODIFY `id` int(11) NOT NULL AUTO_INCREMENT, AUTO_INCREMENT=3;\n\nALTER TABLE `posts`\n  ADD CONSTRAINT `posts_ibfk_1` FOREIGN KEY (`user_id`) REFERENCES `users` (`id`);\nCOMMIT;\n";
    let v = read(dump.as_bytes());
    assert_eq!(fact(&v, "Made by").unwrap(), "phpMyAdmin 5.2.1");
    assert_eq!(fact(&v, "Server").unwrap(), "10.4.32-MariaDB");
    assert_eq!(fact(&v, "Dumped").unwrap(), "Jan 15, 2024 at 10:22 AM");
    let users = table(&v, "shop.users");
    assert_eq!(users["rows"], 2);
    assert_eq!(users["primary_key"], json!(["id"]));
    assert_eq!(users["columns"][0]["auto"], true);
    assert_eq!(users["indexes"][0]["name"], "email");
    let posts = table(&v, "shop.posts");
    assert_eq!(posts["indexes"][0]["name"], "user_id");
    assert_eq!(posts["foreign_keys"][0]["name"], "posts_ibfk_1");
    assert_eq!(posts["foreign_keys"][0]["ref"], "shop.users");
}

#[test]
fn a_dump_cut_off_mid_insert_counts_whole_rows_and_says_so() {
    let cut = MYSQL.find("(3,'cy@").unwrap() + 12;
    let v = read(&MYSQL.as_bytes()[..cut]);
    assert_eq!(v["complete"], true);
    assert_eq!(v["truncated"], true);
    assert!(v["cut"].as_str().unwrap().contains("quoted string"));
    assert_eq!(table(&v, "shop.users")["rows"], 2);
    // cut between statements: the missing footer gives it away
    let cut = MYSQL.find("DROP TABLE IF EXISTS `orders`").unwrap();
    let v = read(&MYSQL.as_bytes()[..cut]);
    assert_eq!(v["truncated"], true);
    assert!(v["notes"][0].as_str().unwrap().contains("Dump completed"));
    assert_eq!(table(&v, "shop.users")["rows"], 4);
}

#[test]
fn gzip_and_a_damaged_gzip() {
    use flate2::{Compression, write::GzEncoder};
    let mut z = GzEncoder::new(Vec::new(), Compression::default());
    z.write_all(PG.as_bytes()).unwrap();
    let gz = z.finish().unwrap();
    let f = tmp(&gz, ".sql.gz");
    let v = schema(1, f.path(), None).unwrap();
    assert_eq!(v["compression"], "gzip");
    assert_eq!(v["complete"], true);
    assert_eq!(table(&v, "public.users")["rows"], 3);
    let f = tmp(&gz[..gz.len() * 2 / 3], ".sql.gz");
    let v = schema(1, f.path(), None).unwrap();
    assert_eq!(v["complete"], true);
    assert_eq!(v["truncated"], true);
}

#[test]
fn not_a_dump() {
    let f = tmp(b"PGDMP\x01\x0e\x00\x04\x08\x01\x01\x01", ".dump");
    assert_eq!(
        schema(1, f.path(), None).unwrap()["format"],
        "pg_dump-custom"
    );
    let f = tmp(b"\x00\x01\x02binary", ".sql");
    assert_eq!(schema(1, f.path(), None).unwrap()["format"], "binary");
}

/// A read in many short slices adds up to the same as one long one, and parks in between.
#[test]
fn reading_in_slices_matches_reading_at_once() {
    let mut dump = String::from(MYSQL.split("-- Dump completed").next().unwrap());
    for i in 0..20_000 {
        dump.push_str(&format!(
            "INSERT INTO `orders` VALUES ({i},1,{i}.5,'note; with ''quotes'' and (parens) {}'),({},2,0,NULL);\n",
            "x".repeat(i % 300),
            i + 100_000
        ));
    }
    dump.push_str("-- Dump completed on 2024-01-15 10:22:33\n");
    let whole = read(dump.as_bytes());
    let f = tmp(dump.as_bytes(), ".sql");
    let mut p = Box::new(Parser::open(f.path()).unwrap());
    let mut slices = 0;
    let v = loop {
        slices += 1;
        if p.run(Instant::now()) {
            break p.json(true);
        }
        let token = park(7, p).unwrap();
        assert!(resume(8, &token).is_none(), "another file's token");
        p = resume(7, &token).unwrap();
    };
    assert!(slices > 2, "{slices}");
    for key in ["shop.users", "shop.orders"] {
        assert_eq!(table(&v, key)["rows"], table(&whole, key)["rows"]);
        assert_eq!(table(&v, key)["bytes"], table(&whole, key)["bytes"]);
    }
    assert_eq!(table(&v, "shop.orders")["rows"], 3 + 40_000);
    assert_eq!(v["truncated"], false);
}
