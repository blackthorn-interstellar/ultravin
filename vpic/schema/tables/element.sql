CREATE TABLE vpic.element (
    id integer NOT NULL,
    name character varying(100) NOT NULL,
    code character varying(50),
    lookuptable character varying(50),
    description character varying,
    isprivate boolean,
    groupname character varying(100),
    datatype character varying(50),
    minallowedvalue integer,
    maxallowedvalue integer,
    isqs boolean,
    decode character varying(50),
    weight integer
);
